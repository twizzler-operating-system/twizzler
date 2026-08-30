use std::{
    ffi::c_void,
    io::{ErrorKind, SeekFrom},
    mem::ManuallyDrop,
    net::Shutdown,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, RwLock,
    },
};

use bitflags::bitflags;
use kinds::socket::SocketKind;
use lazy_static::lazy_static;
use monitor_api::{get_comp_config, CompartmentHandle};
use naming_core::{
    dynamic::{dynamic_naming_factory, DynamicNamingHandle},
    GetFlags, NsNodeKind,
};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::ThreadSyncSleep,
};
use twizzler_io::pty::{PtyServerHandle, PtySignal};
use twizzler_rt_abi::{
    bindings::{
        binding_info, create_options, endpoint, io_ctx, iovec, object_bind_info, open_kind,
        open_kind_OpenKind_KernelConsole, socket_address, wait_kind, BIND_DATA_MAX, FD_CMD_DUP,
        FD_CMD_DUP2, FD_CMD_GET_CLOEXEC, FD_CMD_SET_CLOEXEC, FD_CMD_SYNC, IO_REGISTER_IO_FLAGS,
        OPEN_FLAG_READ, OPEN_FLAG_WRITE,
    },
    error::{ArgumentError, NamingError, ResourceError, TwzError},
    fd::{FdInfo, NameRoot, OpenKind, RawFd, SocketAddress},
    io::{Endpoint, IoFlags},
    Result,
};

use super::{ReferenceRuntime, OUR_RUNTIME};
use crate::runtime::file::kinds::kconsole::KernelConsoleFile;

mod file_desc;
mod kinds;
mod kqueue;
mod poll;
mod select;

pub use kqueue::KqueueFile;

pub type FdImpl = Arc<dyn Fd + Send + Sync + 'static>;

/// Result of [Fd::waitpoint]. `keepalive`, when present, must be held alive by the caller for
/// as long as `sleep`'s underlying memory may still be read (e.g. across a blocking
/// sys_thread_sync call) -- some Fd kinds (sockets) back their wait word with a value that can
/// otherwise be freed out from under a stale reference on handle reuse.
pub struct WaitpointResult {
    pub sleep: ThreadSyncSleep,
    pub ready: bool,
    pub keepalive: Option<Arc<AtomicU64>>,
}

pub trait Fd {
    fn read(
        &self,
        buf: &mut [u8],
        flags: IoFlags,
        offset: Option<u64>,
        ep: Option<&mut Endpoint>,
    ) -> Result<usize>;

    fn write(
        &self,
        buf: &[u8],
        flags: IoFlags,
        offset: Option<u64>,
        to: Option<&Endpoint>,
    ) -> Result<usize>;

    fn seek(&self, _pos: SeekFrom) -> Result<usize> {
        Ok(0)
    }

    fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn stat(&self) -> Result<FdInfo>;

    fn fd_cmd(&self, _cmd: u32, _arg: *const u8, _ret: *mut u8) -> Result<()> {
        Ok(())
    }

    fn get_config(&self, _reg: u32, _val: *mut c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn set_config(&self, _reg: u32, _val: *const c_void, _val_len: usize) -> Result<()> {
        Err(ErrorKind::Unsupported.into())
    }

    fn waitpoint(&self, _kind: wait_kind) -> Result<WaitpointResult> {
        Err(ErrorKind::Unsupported.into())
    }

    /// The inverse of [Fd::waitpoint]: a waitpoint that fires when this fd stops being ready for
    /// `kind` -- the falling edge. `WaitpointResult::ready` is correspondingly "is currently NOT
    /// ready".
    ///
    /// Edge-triggered kqueue registrations (EV_CLEAR) use this to wait out a readiness they have
    /// already reported, instead of re-reporting it every call. Kinds that can't express it leave
    /// this unimplemented, and EV_CLEAR degrades to level-triggered for them -- which spins a
    /// consumer that never drains, but never loses an event.
    fn waitpoint_not_ready(&self, _kind: wait_kind) -> Result<WaitpointResult> {
        Err(ErrorKind::Unsupported.into())
    }

    fn shutdown(&self, _sh: Shutdown) -> Result<()> {
        Ok(())
    }

    fn as_socket(&self) -> Option<&SocketKind> {
        None
    }

    fn as_kqueue(&self) -> Option<&KqueueFile> {
        None
    }

    fn close(&self) -> Result<()> {
        self.shutdown(Shutdown::Both)
    }

    fn dup(&self) -> Option<FdImpl> {
        None
    }
}

/// Extract the optional file offset from an `io_ctx`. Returns `None` when the offset is `FD_POS`
/// (meaning "use the fd's current position").
fn io_ctx_offset(ctx: *mut io_ctx) -> Option<u64> {
    let raw_offset = if ctx.is_null() {
        twizzler_rt_abi::bindings::FD_POS
    } else {
        unsafe { (*ctx).offset }
    };
    if raw_offset == twizzler_rt_abi::bindings::FD_POS {
        None
    } else {
        Some(raw_offset as u64)
    }
}

#[derive(Clone)]
struct MaybeNoDrop<T> {
    pub should_drop: bool,
    t: ManuallyDrop<T>,
}

impl<T> Deref for MaybeNoDrop<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.t.deref()
    }
}

impl<T> MaybeNoDrop<T> {
    fn new(t: T, should_drop: bool) -> Self {
        Self {
            should_drop,
            t: ManuallyDrop::new(t),
        }
    }
}

impl<T> AsRef<T> for MaybeNoDrop<T> {
    fn as_ref(&self) -> &T {
        &self.t
    }
}

impl<T> Drop for MaybeNoDrop<T> {
    fn drop(&mut self) {
        if self.should_drop {
            unsafe { ManuallyDrop::<T>::drop(&mut self.t) };
        }
    }
}

#[derive(Clone)]
struct FileDesc {
    file: FdImpl,
    binding: MaybeNoDrop<Arc<binding_info>>,
    flags: Arc<AtomicU32>,
    /// Close-on-exec. Per-descriptor, not per-description: the dup paths below install a fresh
    /// cell rather than sharing this one, because POSIX has dup() clear the flag on the copy.
    cloexec: Arc<AtomicBool>,
}

impl FileDesc {
    fn io_ctx_flags(&self, ctx: *mut io_ctx) -> IoFlags {
        let flags = IoFlags::from_bits_truncate(self.flags.load(Ordering::SeqCst))
            | if ctx.is_null() {
                IoFlags::empty()
            } else {
                IoFlags::from_bits_truncate(unsafe { (*ctx).flags })
            };
        flags
    }

    pub fn new(
        file: FdImpl,
        bind_kind: open_kind,
        flags: u32,
        bind_info: Option<&[u8]>,
        should_drop: bool,
    ) -> Self {
        let bind_len = bind_info.map_or(0, |bi| bi.len()).min(BIND_DATA_MAX);
        let mut binding = binding_info {
            kind: bind_kind,
            fd: 0,
            flags,
            bind_data: [0; _],
            bind_len: bind_len as u32,
        };
        if let Some(bind_info) = bind_info {
            binding.bind_data[0..bind_len].copy_from_slice(&bind_info[0..bind_len])
        }
        FileDesc {
            file,
            binding: MaybeNoDrop::new(Arc::new(binding), should_drop),
            flags: Arc::new(AtomicU32::new(0)),
            cloexec: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn seek(&self, pos: SeekFrom) -> Result<usize> {
        self.file.seek(pos).into()
    }

    pub fn stat(&self) -> Result<FdInfo> {
        self.file.stat().into()
    }

    pub fn fd_cmd(&mut self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        if cmd == twizzler_rt_abi::bindings::FD_CMD_SHUTDOWN {
            let val = unsafe { arg.cast::<u32>().read() };
            let shutdown = match val {
                0 => return Err(TwzError::INVALID_ARGUMENT),
                1 => std::net::Shutdown::Read,
                2 => std::net::Shutdown::Write,
                _ => std::net::Shutdown::Both,
            };
            let mut b = **self.binding;
            let flags = match shutdown {
                Shutdown::Read => b.flags & !OPEN_FLAG_READ,
                Shutdown::Write => b.flags & !OPEN_FLAG_WRITE,
                Shutdown::Both => b.flags & !(OPEN_FLAG_READ | OPEN_FLAG_WRITE),
            };
            b.flags = flags;
            self.binding = MaybeNoDrop::new(Arc::new(b), true);
            self.file.shutdown(shutdown)?;
            return Ok(());
        } else if cmd == FD_CMD_SYNC {
            self.file.flush()?;
            return Ok(());
        }
        self.file.fd_cmd(cmd, arg, ret).into()
    }

    fn pread(&mut self, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.read(buf, flags, offset, None)
    }

    fn pwrite(&mut self, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.write(buf, flags, offset, None)
    }

    fn pread_from(
        &mut self,
        buf: &mut [u8],
        ctx: *mut io_ctx,
        ep: &mut twizzler_rt_abi::io::Endpoint,
    ) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.read(buf, flags, offset, Some(ep))
    }

    fn pwrite_to(
        &mut self,
        buf: &[u8],
        ctx: *mut io_ctx,
        ep: &twizzler_rt_abi::io::Endpoint,
    ) -> Result<usize> {
        let offset = io_ctx_offset(ctx);
        let flags = self.io_ctx_flags(ctx);
        self.file.write(buf, flags, offset, Some(ep))
    }
}

const MAX_FD: usize = 1024;

struct FdSlots {
    slots: [Option<FileDesc>; MAX_FD],
}

impl FdSlots {
    pub fn insert(&mut self, idx: usize, elem: FileDesc) -> Option<FileDesc> {
        self.slots[idx].replace(elem)
    }

    pub fn insert_first_empty(&mut self, elem: FileDesc) -> Option<usize> {
        for i in 0..MAX_FD {
            if self.slots[i].is_none() {
                self.insert(i, elem);
                return Some(i);
            }
        }
        None
    }

    pub fn get(&self, idx: usize) -> Option<&FileDesc> {
        self.slots.get(idx).and_then(Option::as_ref)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut FileDesc> {
        self.slots.get_mut(idx).and_then(Option::as_mut)
    }

    pub fn remove(&mut self, idx: usize) -> Option<FileDesc> {
        self.slots[idx].take()
    }
}

lazy_static! {
    // RwLock, not Mutex: lookups (every read/write/pread on every thread in the compartment)
    // only `get` and clone a descriptor, so they share the lock; the few mutation sites
    // (open/close/dup/init) take it exclusively.
    static ref FD_SLOTS: RwLock<FdSlots> = {
        let mut slots = FdSlots {
            slots: [const { None }; MAX_FD],
        };
        slots.insert(
            0,
            FileDesc::new(
                Arc::new(KernelConsoleFile::new()),
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        slots.insert(
            1,
            FileDesc::new(
                Arc::new(KernelConsoleFile::new()),
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        slots.insert(
            2,
            FileDesc::new(
                Arc::new(KernelConsoleFile::new()),
                open_kind_OpenKind_KernelConsole,
                0,
                None,
                false,
            ),
        );
        RwLock::new(slots)
    };
}

/// The one naming handle for this runtime, and a latch recording that naming came up at all.
///
/// This used to be a sharded pool of handles: the buffer protocol wrote paths at offset 0, so a
/// handle had to be exclusively owned for the duration of a call, and concurrency meant many
/// handles. `NamingHandle` is now `&self`-callable -- short paths cross inline in the gate
/// arguments and longer ones use disjoint slots of the one buffer -- so every thread shares this
/// single handle, and the working namespace really is per-process state on the server instead of
/// a generation-synced property faked across a pool.
static RUNTIME_NAMER: OnceLock<DynamicNamingHandle> = OnceLock::new();
static NAMING_UP: OnceLock<()> = OnceLock::new();

/// This compartment's memo of its working directory.
///
/// Not a second place the cwd lives: the only value ever written here is one the naming server
/// returned (or one we know without asking, see [`cwd_memo_seed_root`]), and the only thing that
/// can move this handle's working namespace is this runtime asking it to. So the memo caches an
/// answer rather than holding an opinion, and it is dropped rather than updated on every move.
///
/// It replaces a `NameRoot::Current` entry in the runtime's nameroot map that was maintained by
/// joining and lexically normalising path strings client-side -- arithmetic that could, and did,
/// come to a different answer than the namespace walk the server performed for the same call.
static CWD_MEMO: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Bumped under [`CWD_MEMO`]'s lock whenever this runtime moves. A reader that fetched a cwd
/// while a move was in flight sees the count change and drops its answer instead of memoizing a
/// path that is already wrong.
static CWD_GEN: AtomicU64 = AtomicU64::new(0);

/// Forget the memoized working directory: this runtime just moved, or re-rooted.
pub(crate) fn cwd_memo_invalidate() {
    let mut memo = CWD_MEMO.lock().unwrap();
    CWD_GEN.fetch_add(1, Ordering::SeqCst);
    *memo = None;
}

/// A bequest this compartment was spawned with, not yet collected.
///
/// Collected on first naming use rather than at startup. A compartment that never names anything
/// never acquires a naming handle at all, and spending a gate call at spawn to set a working
/// directory nothing will read is exactly the cost [`crate::runtime::core`]'s
/// `SKIP_ROOT_NAMESPACE_SET` exists to avoid.
static PENDING_BEQUEST: AtomicU64 = AtomicU64::new(0);

/// Record the bequest named in this compartment's config, to collect on first use.
pub(crate) fn set_pending_bequest(token: u64) {
    PENDING_BEQUEST.store(token, Ordering::SeqCst);
}

/// Mint a bequest carrying this compartment's working namespace, for one it is spawning.
///
/// `None` when there is nothing worth handing over -- no namer, or we are at the root, where the
/// child's default already agrees. Skipping the root case is what keeps a plain spawn free of
/// naming traffic.
pub(crate) fn mint_cwd_bequest() -> Option<u64> {
    if current_dir().ok()? == Path::new("/") {
        return None;
    }
    get_naming_handle()?.bequeath().ok()
}

/// This compartment's working directory, without the trip out through libstd.
///
/// `canon_name` used to reach this value with `std::env::current_dir()`, which is
/// `twz_rt_get_nameroot` -> `OUR_RUNTIME.get_nameroot` -- the runtime asking libstd for something
/// it holds in a static one frame below, paying a C-ABI round trip, libstd's grow-and-retry
/// buffer loop and a `PathBuf` allocation on *every relative path it resolves*.
pub(crate) fn current_dir() -> Result<PathBuf> {
    if let Some(path) = CWD_MEMO.lock().unwrap().clone() {
        return Ok(path);
    }
    // The monitor has no working namespace -- it is not a naming client and must not become one
    // while loading a compartment (`CompartmentLoader::new` reads this to seed the child's
    // initial directory).
    if OUR_RUNTIME
        .state()
        .contains(super::RuntimeState::IS_MONITOR)
    {
        return Ok(PathBuf::from("/"));
    }
    let gen = CWD_GEN.load(Ordering::SeqCst);
    // No namer yet (the bootstrap chain: logboi, devmgr, pager, naming itself, init) means there
    // is no working namespace to be anywhere in, and a handle opened once one exists starts at
    // its root. Report "/" rather than failing -- and do not memoize it, so the first reader
    // after the namer is up asks.
    let Some(handle) = get_naming_handle() else {
        return Ok(PathBuf::from("/"));
    };
    let path = handle.cwd()?;
    let mut memo = CWD_MEMO.lock().unwrap();
    // Discard the answer if this runtime moved while we were fetching it; the next reader
    // re-asks rather than being served a path that is already stale.
    if CWD_GEN.load(Ordering::SeqCst) == gen {
        *memo = Some(path.clone());
    }
    Ok(path)
}

/// Seed the memo with a cwd known without asking.
///
/// A naming handle this runtime has not opened yet sits at its root, so a compartment that has
/// not moved is at `/` -- a fact, not a default. This is what lets `SKIP_ROOT_NAMESPACE_SET`
/// keep its saving: a child that never names anything still answers `current_dir()` without
/// acquiring a handle or crossing a gate.
pub(crate) fn cwd_memo_seed_root() {
    let mut memo = CWD_MEMO.lock().unwrap();
    if memo.is_none() {
        *memo = Some(PathBuf::from("/"));
    }
}

#[track_caller]
fn get_fd_slots() -> &'static RwLock<FdSlots> {
    &FD_SLOTS
}

pub fn get_naming_handle() -> Option<&'static DynamicNamingHandle> {
    if let Some(handle) = RUNTIME_NAMER.get() {
        return Some(handle);
    }
    // Handle creation is once per process now, but never latch a failure: this compartment may
    // predate the namer (init does) and must succeed on retry once it is up.
    let _diag = crate::runtime::core::PRE_MAIN_PHASE_STATS;
    let _t0 = std::time::Instant::now();
    if NAMING_UP.get().is_none() {
        // Weakly-bound gates prove naming-srv was loaded before this compartment, so the
        // monitor lookup -- one gate call -- is only spent when this compartment predates
        // the namer.
        if !naming_core::gates::bound() && CompartmentHandle::lookup("naming").is_err() {
            return None;
        }
        let _ = NAMING_UP.set(());
    }
    let _t_lookup = _t0.elapsed();
    let handle = dynamic_naming_factory()?;
    secgate::statlog::record_on(
        _diag,
        "NAMEHDL",
        _t0.elapsed().as_micros() as u64,
        &[
            _t_lookup.as_micros() as u64,
            (_t0.elapsed() - _t_lookup).as_micros() as u64,
        ],
    );
    // A racing initializer loses here; its handle drops and closes its descriptor.
    if RUNTIME_NAMER.set(handle).is_ok() {
        // Only the winner collects. `swap` makes the token single-use on this side too, so a
        // loser's about-to-be-dropped handle cannot consume the bequest first and strand the
        // handle everyone else will use at the root.
        let token = PENDING_BEQUEST.swap(0, Ordering::SeqCst);
        if token != 0 {
            // Unwrap-Ok: set immediately above, by us.
            let _ = RUNTIME_NAMER.get().unwrap().redeem_bequest(token);
            // We inherited a working namespace but not its name; the first reader asks.
            cwd_memo_invalidate();
        }
    }
    RUNTIME_NAMER.get()
}

/// Set the working namespace for this compartment: per-descriptor state on the server, and the
/// compartment holds exactly one descriptor.
pub fn set_naming_namespace(path: &std::path::Path) -> Result<()> {
    let _t0 = std::time::Instant::now();
    let handle = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
    let _t_handle = _t0.elapsed();
    handle.change_namespace(path)?;
    cwd_memo_invalidate();
    // Called once per compartment from `pre_main_hook`, i.e. inside `Command::spawn`. Splits
    // acquiring the naming handle (for a fresh compartment: possibly a compartment lookup, plus
    // open_handle) from the namespace call itself.
    secgate::statlog::record_on(
        crate::runtime::core::PRE_MAIN_PHASE_STATS,
        "SETNS",
        _t0.elapsed().as_micros() as u64,
        &[
            _t_handle.as_micros() as u64,
            (_t0.elapsed() - _t_handle).as_micros() as u64,
        ],
    );
    Ok(())
}

#[derive(Debug)]
pub enum CreateOptions {
    UNEXPECTED,
    CreateKindExisting,
    CreateKindNew,
    CreateKindEither,
    CreateKindBind(ObjID),
}

impl From<create_options> for CreateOptions {
    fn from(value: create_options) -> Self {
        match value.kind {
            twizzler_rt_abi::bindings::CREATE_KIND_EITHER => CreateOptions::CreateKindEither,
            twizzler_rt_abi::bindings::CREATE_KIND_NEW => {
                if value.id != 0 {
                    CreateOptions::CreateKindBind(value.id.into())
                } else {
                    CreateOptions::CreateKindNew
                }
            }
            twizzler_rt_abi::bindings::CREATE_KIND_EXISTING => CreateOptions::CreateKindExisting,
            _ => CreateOptions::UNEXPECTED,
        }
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct OperationOptions: u32 {
        const OPEN_FLAG_READ = twizzler_rt_abi::bindings::OPEN_FLAG_READ;
        const OPEN_FLAG_WRITE = twizzler_rt_abi::bindings::OPEN_FLAG_WRITE;
        const OPEN_FLAG_TRUNCATE = twizzler_rt_abi::bindings::OPEN_FLAG_TRUNCATE;
        const OPEN_FLAG_TAIL = twizzler_rt_abi::bindings::OPEN_FLAG_TAIL;
        const OPEN_FLAG_SYMLINK = twizzler_rt_abi::bindings::OPEN_FLAG_SYMLINK;
    }
}

impl From<u32> for OperationOptions {
    fn from(value: u32) -> Self {
        OperationOptions::from_bits_truncate(value)
    }
}

fn pty_signal_handler(server: &PtyServerHandle, sig: PtySignal) {
    let signal = match sig {
        PtySignal::Interrupt => libc::SIGINT,
        PtySignal::Quit => libc::SIGQUIT,
        PtySignal::Suspend => libc::SIGTSTP,
        PtySignal::Status => libc::SIGINFO,
        PtySignal::Winch => libc::SIGWINCH,
    } as u64;
    let _ = monitor_api::post_signal(
        Some(server.object().id()),
        signal,
        monitor_api::PostSignalFlags::CONTROLLER,
    )
    .inspect_err(|e| {
        tracing::warn!(
            "failed to raise signal for controller {}: {}",
            server.object().id(),
            e
        )
    });
}

/// Defer a stdio bind's `open` until something actually uses the descriptor.
///
/// `init_fds` opens every bind eagerly: an object map plus handler registration per bind, measured
/// at ~165 us of a spawn (`PREMAIN`, spawnbench.md §47) for descriptors a short-lived program may
/// never touch -- `nullexit` touches none of them. This wrapper sits in the fd slot in place of the
/// real `Fd`, so none of the 22 `get_fd_slots()` call sites change, and materializes on the first
/// operation that needs a real one.
///
/// `close()` on a bind that was never used stays a no-op, which is what makes the saving survive
/// `close_fds` at exit rather than merely moving it there.
///
/// Failure is cached as `None`: a bind that cannot be opened reports an error on every use instead
/// of retrying the failing open per call. That is the one visible semantic change -- an open error
/// that used to be printed during startup now surfaces at first use.
/// **SHIPPED on (2026-08-26), on hygiene grounds rather than measured speed** -- spawnbench.md
/// §59-64. What is established is a **count**: opens per spawn **4.47 -> 1.18**, i.e. 3.29 opens
/// eliminated, with relocations/spawn flat across the arms as a control. What is *not* established
/// is any wall-clock win: the clean A/B/A read -0.41% against one baseline and **+1.85% against the
/// other**, inside a 2.22% drift floor -- the sign reverses with the choice of baseline, so there
/// is no time effect to claim. Validated by boot (58/58 with this on), not merely compiled.
///
/// **Semantic change:** an open error that used to print during startup now surfaces at **first
/// use** of the descriptor, and failure is cached rather than retried per call. A program that
/// binds a bad fd and never touches it will never see the error.
///
/// Those measurements predate TLBFIX, the pipe-EOF work and the predicate alignment; do not treat
/// the null as current without re-measuring.
pub const LAZY_FDS: bool = true;

struct LazyBind {
    kind: OpenKind,
    opts: OperationOptions,
    bind: Vec<u8>,
    inner: std::sync::OnceLock<Option<FdImpl>>,
}

impl LazyBind {
    /// Pipe is excluded: `ReferenceRuntime::open` applies per-direction shutdown to a pipe elem
    /// after opening it, and reproducing that here would duplicate logic rather than defer it.
    /// Same exclusion, and the same reason, as the dedupe whitelist below.
    fn eligible(kind: OpenKind) -> bool {
        !matches!(kind, OpenKind::Pipe)
    }

    fn get(&self) -> Option<&FdImpl> {
        self.inner
            .get_or_init(|| {
                kinds::open(
                    None,
                    self.kind,
                    self.bind.as_ptr().cast(),
                    self.bind.len(),
                    self.opts,
                )
                .inspect_err(|e| {
                    twizzler_abi::klog_println!("lazy bind open failed ({:?}): {}", self.kind, e)
                })
                .ok()
                .flatten()
            })
            .as_ref()
    }

    fn fd(&self) -> Result<&FdImpl> {
        self.get().ok_or(TwzError::NOT_SUPPORTED)
    }
}

impl Fd for LazyBind {
    fn read(
        &self,
        buf: &mut [u8],
        flags: IoFlags,
        offset: Option<u64>,
        ep: Option<&mut Endpoint>,
    ) -> Result<usize> {
        self.fd()?.read(buf, flags, offset, ep)
    }
    fn write(
        &self,
        buf: &[u8],
        flags: IoFlags,
        offset: Option<u64>,
        to: Option<&Endpoint>,
    ) -> Result<usize> {
        self.fd()?.write(buf, flags, offset, to)
    }
    fn seek(&self, pos: SeekFrom) -> Result<usize> {
        self.fd()?.seek(pos)
    }
    fn flush(&self) -> Result<()> {
        self.fd()?.flush()
    }
    fn stat(&self) -> Result<FdInfo> {
        self.fd()?.stat()
    }
    fn fd_cmd(&self, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        self.fd()?.fd_cmd(cmd, arg, ret)
    }
    fn get_config(&self, reg: u32, val: *mut c_void, val_len: usize) -> Result<()> {
        self.fd()?.get_config(reg, val, val_len)
    }
    fn set_config(&self, reg: u32, val: *const c_void, val_len: usize) -> Result<()> {
        self.fd()?.set_config(reg, val, val_len)
    }
    fn waitpoint(&self, kind: wait_kind) -> Result<WaitpointResult> {
        self.fd()?.waitpoint(kind)
    }
    fn waitpoint_not_ready(&self, kind: wait_kind) -> Result<WaitpointResult> {
        self.fd()?.waitpoint_not_ready(kind)
    }
    fn shutdown(&self, sh: Shutdown) -> Result<()> {
        self.fd()?.shutdown(sh)
    }
    fn as_socket(&self) -> Option<&SocketKind> {
        self.get()?.as_socket()
    }
    fn as_kqueue(&self) -> Option<&KqueueFile> {
        self.get()?.as_kqueue()
    }
    /// The whole point: an unused bind was never opened, so there is nothing to close.
    fn close(&self) -> Result<()> {
        match self.inner.get() {
            Some(Some(fd)) => fd.close(),
            _ => Ok(()),
        }
    }
}

impl ReferenceRuntime {
    pub(crate) fn close_fds(&self) {
        for (_i, fd) in get_fd_slots().write().unwrap().slots.iter_mut().enumerate() {
            if let Some(fd) = fd.take() {
                let _ = fd.file.close();
                drop(fd);
            }
        }
    }

    pub(crate) fn init_fds(&self) {
        let loader_config = &get_comp_config().loader_config;

        if loader_config.fd_spec.is_null() {
            return;
        }

        let slice = unsafe {
            core::slice::from_raw_parts::<binding_info>(
                loader_config.fd_spec,
                loader_config.fd_spec_len,
            )
        };

        // The stdio binds are usually three references to one object (the console PTY), and each
        // PTY open pays an object map plus handler registration -- measured at most of
        // init_fds's ~147us (`PREMAIN`, spawnbench.md §30). Share the underlying Fd across
        // identical (kind, bind-bytes) binds instead. Only PTY kinds: sharing is exactly how a
        // Unix tty's three stdio fds behave, and registering the signal handler once is the
        // correct count. Pipes must not share (open() applies per-direction shutdown to the
        // elem), and Path files must not (independent seek cursors).
        let mut opened: Vec<(open_kind, &[u8], FdImpl)> = Vec::new();
        for bi in slice {
            let Ok(kind) = OpenKind::try_from(bi.kind) else {
                continue;
            };
            // Deliberately no `bi.fd > 2` filter. That guard was correct when stdio was the
            // only thing that could cross a spawn, and became a silent discard once the parent
            // began sending a binding for every open descriptor (`read_binds` walks all slots).
            // Everything upstream of here already works: the parent builds the binding, it
            // marshals through exec_spawn -> with_fd_specs -> loader_config.fd_spec, and this
            // loop receives it -- dropping a jobserver pipe or a shell's `exec 3>file`
            // redirection with nothing reported at either end.
            let bytes = &bi.bind_data[..(bi.bind_len as usize).min(bi.bind_data.len())];
            // Lazy path first: it subsumes the dedupe below (two binds that would have shared an
            // Fd now each cost nothing until used) and skips the open entirely for a program that
            // never touches the descriptor.
            if LAZY_FDS && LazyBind::eligible(kind) {
                let elem: FdImpl = std::sync::Arc::new(LazyBind {
                    kind,
                    opts: OperationOptions::from_bits_truncate(bi.flags),
                    bind: bytes.to_vec(),
                    inner: std::sync::OnceLock::new(),
                });
                let fdesc = FileDesc::new(
                    elem,
                    bi.kind,
                    OperationOptions::from_bits_truncate(bi.flags).bits(),
                    Some(bytes),
                    false,
                );
                get_fd_slots()
                    .write()
                    .unwrap()
                    .insert(bi.fd as usize, fdesc);
                continue;
            }
            let dedupable = matches!(kind, OpenKind::PtyClient | OpenKind::PtyServer);
            if dedupable {
                if let Some(elem) = opened
                    .iter()
                    .find(|(k, b, _)| *k == bi.kind && *b == bytes)
                    .map(|(_, _, e)| e.clone())
                {
                    let fdesc = FileDesc::new(
                        elem,
                        bi.kind,
                        OperationOptions::from_bits_truncate(bi.flags).bits(),
                        Some(bytes),
                        false,
                    );
                    if get_fd_slots()
                        .write()
                        .unwrap()
                        .insert(bi.fd as usize, fdesc)
                        .is_some()
                    {
                        continue;
                    }
                    // Insert failed; fall through to the ordinary open path.
                }
            }
            let r = self
                .open(
                    Some(bi.fd),
                    kind,
                    OperationOptions::from_bits_truncate(bi.flags),
                    bi.bind_data.as_ptr().cast(),
                    bi.bind_len as usize,
                    false,
                )
                .inspect_err(|e| {
                    twizzler_abi::klog_println!("Failed to open fd ({}): {}", bi.fd, e);
                });
            if dedupable {
                if let Ok(fd) = r {
                    if let Some(f) = get_fd_slots().read().unwrap().get(fd as usize) {
                        opened.push((bi.kind, bytes, f.file.clone()));
                    }
                }
            }
        }
    }

    pub fn canon_name(
        &self,
        resolver: twizzler_rt_abi::fd::NameResolver,
        name: &[u8],
        out_name: &mut [u8],
    ) -> Result<usize> {
        if matches!(resolver, twizzler_rt_abi::fd::NameResolver::Socket) {
            let Ok(name) = str::from_utf8(name) else {
                return Err(TwzError::INVALID_ARGUMENT);
            };
            let out_slice: &mut [socket_address] = unsafe {
                core::slice::from_raw_parts_mut(
                    out_name.as_mut_ptr().cast(),
                    out_name.len() / size_of::<socket_address>(),
                )
            };

            let res = crate::runtime::file::kinds::socket::dns(name)?;
            for i in 0..res.len().min(out_slice.len()) {
                let sa = SocketAddress::from(res[i]);
                out_slice[i] = sa.0;
            }
            return Ok(res.len().min(out_slice.len()) * size_of::<socket_address>());
        }
        let path = PathBuf::from(str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT)?);
        let path = if !path.is_absolute() {
            let mut cd = current_dir()?;
            cd.push(path);
            cd
        } else {
            path
        };

        let npath = path.normalize_lexically().unwrap_or(path);
        let path = npath.to_str().unwrap().as_bytes();

        let len = out_name.len().min(path.len());
        out_name[0..len].copy_from_slice(&path[0..len]);
        Ok(len)
    }

    pub fn resolve_name(
        &self,
        _resolver: twizzler_rt_abi::fd::NameResolver,
        name: &[u8],
    ) -> Result<ObjID> {
        let name = str::from_utf8(name).map_err(|_| TwzError::INVALID_ARGUMENT)?;
        // One acquire, not two. The handle used to be borrowed once to test whether naming was up,
        // dropped, and borrowed again to do the work -- two round trips through the pool's lock on
        // the hottest naming call in the system, for a question the first borrow's result already
        // answers.
        let Some(session) = get_naming_handle() else {
            fn get_kernel_init_info() -> &'static KernelInitInfo {
                unsafe {
                    (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
                        as *const KernelInitInfo)
                        .as_ref()
                        .unwrap()
                }
            }

            fn find_init_name(name: &str) -> Option<(ObjID, String)> {
                let init_info = get_kernel_init_info();
                for n in init_info.names() {
                    if n.name() == name {
                        return Some((n.id(), name.to_string()));
                    }
                }
                None
            }
            let id = find_init_name(name).ok_or(NamingError::NotFound)?;
            return Ok(id.0);
        };
        let res = session.get(name, GetFlags::FOLLOW_SYMLINK)?;
        tracing::trace!("resolve got {:?}", res);
        Ok(res.id)
    }

    pub fn mkns(&self, name: &str) -> Result<()> {
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;

        session.put_namespace(name, true)?;
        Ok(())
    }

    pub fn symlink(&self, name: &str, target: &str) -> Result<()> {
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;

        session.symlink(name, target)?;
        Ok(())
    }

    pub fn readlink(&self, name: &str, target: &mut [u8], read_len: &mut u64) -> Result<()> {
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
        let node = session.get(name, GetFlags::empty())?;

        let link = node.readlink()?;
        let len = target.len().min(link.as_bytes().len());
        target[0..len].copy_from_slice(&link.as_bytes()[0..len]);
        *read_len = len as u64;
        Ok(())
    }

    pub fn read_binds(&self, binds: &mut [binding_info]) -> usize {
        let bindings = get_fd_slots().read().unwrap();
        let mut idx = 0;
        for (fd, info) in bindings.slots.iter().enumerate() {
            if idx >= binds.len() {
                return idx;
            }
            if let Some(info) = info {
                binds[idx] = **info.binding;
                binds[idx].fd = fd.try_into().unwrap();
                idx += 1;
            }
        }
        return idx;
    }

    pub fn open(
        &self,
        existing_fd: Option<RawFd>,
        kind: OpenKind,
        open_opt: OperationOptions,
        bind_info: *const c_void,
        bind_info_len: usize,
        should_drop: bool,
    ) -> Result<RawFd> {
        let bind_info_bytes = if bind_info.is_null() {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bind_info.cast::<u8>(), bind_info_len) }
        };
        let existing_flags = if kind == OpenKind::SocketConnect && existing_fd.is_some() {
            let slots = get_fd_slots().read().unwrap();
            if let Some(fd) = slots.get(existing_fd.unwrap() as usize) {
                Some(fd.flags.load(Ordering::SeqCst))
            } else {
                None
            }
        } else {
            None
        };
        let t_open = std::time::Instant::now();
        let elem = kinds::open(existing_fd, kind, bind_info, bind_info_len, open_opt)?;
        let kinds_ns = t_open.elapsed().as_nanos() as u64;

        if elem.is_none() && existing_fd.is_none() {
            return Err(TwzError::NOT_SUPPORTED);
        }

        if elem.is_none() {
            return Ok(existing_fd.unwrap());
        }
        let elem = elem.unwrap();

        let elem = match kind {
            OpenKind::Pipe => {
                let binding_info = object_bind_info {
                    id: elem.stat()?.id,
                };
                let bind_info_bytes = unsafe {
                    core::slice::from_raw_parts(
                        &binding_info as *const object_bind_info as *const u8,
                        std::mem::size_of::<object_bind_info>(),
                    )
                };

                if !open_opt.contains(OperationOptions::OPEN_FLAG_READ)
                    && !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE)
                {
                    tracing::error!(
                        "Invalid open options for pipe: must specify at least one of read or write"
                    );
                }
                if !open_opt.contains(OperationOptions::OPEN_FLAG_READ) {
                    let _ = elem.shutdown(Shutdown::Read);
                }

                if !open_opt.contains(OperationOptions::OPEN_FLAG_WRITE) {
                    let _ = elem.shutdown(Shutdown::Write);
                }

                FileDesc::new(
                    elem,
                    kind as u32,
                    open_opt.bits(),
                    Some(bind_info_bytes),
                    should_drop,
                )
            }
            _ => FileDesc::new(
                elem,
                kind as u32,
                open_opt.bits(),
                Some(bind_info_bytes),
                should_drop,
            ),
        };
        if let Some(existing_flags) = existing_flags {
            elem.flags.store(existing_flags, Ordering::SeqCst);
        }

        let t_fd = std::time::Instant::now();
        let mut binding = get_fd_slots().write().unwrap();

        let fd = if let Some(fd) = existing_fd {
            binding.insert(fd.try_into().unwrap(), elem);
            Some(fd as usize)
        } else {
            binding.insert_first_empty(elem)
        }
        .ok_or(ResourceError::OutOfNames)?;

        drop(binding);
        kinds::openstats::record_outer(
            kinds_ns,
            t_fd.elapsed().as_nanos() as u64,
            t_open.elapsed().as_nanos() as u64,
        );
        if open_opt.contains(OperationOptions::OPEN_FLAG_TAIL) {
            self.seek(fd.try_into().unwrap(), SeekFrom::End(0))?;
        }
        Ok(fd.try_into().unwrap())
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<()> {
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
        Ok(session.rename(old, new)?)
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
        Ok(session.remove(path)?)
    }

    pub fn read(&self, fd: RawFd, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        let len = file_desc
            .file
            .read(buf, file_desc.io_ctx_flags(ctx), None, None)?;
        Ok(len)
    }

    pub fn fd_pread_from(
        &self,
        fd: RawFd,
        buf: &mut [u8],
        ctx: *mut io_ctx,
        ep: *mut endpoint,
    ) -> Result<usize> {
        let ep = unsafe { ep.cast::<twizzler_rt_abi::io::Endpoint>().as_mut().unwrap() };
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pread_from(buf, ctx, ep)?)
    }

    pub fn fd_pwrite_to(
        &self,
        fd: RawFd,
        buf: &[u8],
        ctx: *mut io_ctx,
        ep: *const endpoint,
    ) -> Result<usize> {
        let ep = unsafe { ep.cast::<twizzler_rt_abi::io::Endpoint>().as_ref().unwrap() };
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pwrite_to(buf, ctx, ep)?)
    }

    pub fn fd_pread(&self, fd: RawFd, buf: &mut [u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pread(buf, ctx)?)
    }

    pub fn fd_pwrite(&self, fd: RawFd, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        Ok(file_desc.pwrite(buf, ctx)?)
    }

    pub fn fd_pwritev(&self, fd: RawFd, iovs: &[iovec], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        let mut total = 0usize;
        for iov in iovs {
            let slice =
                unsafe { core::slice::from_raw_parts(iov.iov_base.cast::<u8>(), iov.iov_len) };
            total += file_desc.pwrite(slice, ctx)?;
        }
        Ok(total)
    }

    pub fn fd_preadv(&self, fd: RawFd, iovs: &[iovec], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let mut file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);
        let mut total = 0usize;
        for iov in iovs {
            let slice =
                unsafe { core::slice::from_raw_parts_mut(iov.iov_base.cast::<u8>(), iov.iov_len) };
            total += file_desc.pread(slice, ctx)?;
        }
        Ok(total)
    }

    pub fn fd_get_info(&self, fd: RawFd) -> Option<twizzler_rt_abi::bindings::fd_info> {
        let mut binding = get_fd_slots().write().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return None;
        };
        fd.stat().ok().map(|x| x.into())
    }

    pub fn fd_get_config(
        &self,
        fd: RawFd,
        reg: u32,
        val: *mut c_void,
        val_len: usize,
    ) -> Result<()> {
        let mut binding = get_fd_slots().write().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return Err(TwzError::INVALID_ARGUMENT);
        };

        if reg == IO_REGISTER_IO_FLAGS {
            if val_len != size_of::<u32>() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            unsafe { val.cast::<u32>().write(fd.flags.load(Ordering::SeqCst)) };
            return Ok(());
        }

        let buf = unsafe { core::slice::from_raw_parts_mut(val.cast::<u8>(), val_len) };
        buf.fill(0);
        fd.file.get_config(reg, val, val_len).into()
    }

    pub fn fd_set_config(
        &self,
        fd: RawFd,
        reg: u32,
        val: *const c_void,
        val_len: usize,
    ) -> Result<()> {
        let mut binding = get_fd_slots().write().unwrap();
        let Some(fd) = binding.get_mut(fd.try_into().unwrap()) else {
            return Err(TwzError::INVALID_ARGUMENT);
        };

        if reg == IO_REGISTER_IO_FLAGS {
            if val_len != size_of::<u32>() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            let val = unsafe { val.cast::<u32>().read() };
            fd.flags.store(val, Ordering::SeqCst);
            return Ok(());
        }
        fd.file.set_config(reg, val, val_len).into()
    }

    pub fn fd_cmd(&self, fd: RawFd, cmd: u32, arg: *const u8, ret: *mut u8) -> Result<()> {
        let mut binding = get_fd_slots().write().unwrap();
        let file_desc = binding.get_mut(fd.try_into().unwrap());

        let file_desc = file_desc.ok_or(TwzError::INVALID_ARGUMENT)?;

        if cmd == FD_CMD_GET_CLOEXEC {
            if ret.is_null() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            let v = file_desc.cloexec.load(Ordering::SeqCst) as u32;
            unsafe { ret.cast::<u32>().write(v) };
            return Ok(());
        }

        if cmd == FD_CMD_SET_CLOEXEC {
            if arg.is_null() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            let v = unsafe { arg.cast::<u32>().read() };
            file_desc.cloexec.store(v != 0, Ordering::SeqCst);
            return Ok(());
        }

        if cmd == FD_CMD_DUP {
            let file = file_desc
                .file
                .dup()
                .unwrap_or_else(|| file_desc.file.clone());
            let mut nfd = file_desc.clone();
            nfd.file = file;
            nfd.cloexec = Arc::new(AtomicBool::new(false));
            let b = **nfd.binding;
            nfd.binding = MaybeNoDrop::new(Arc::new(b), true);
            let newfd = binding
                .insert_first_empty(nfd)
                .ok_or(ResourceError::OutOfNames)?;
            unsafe {
                ret.cast::<RawFd>().write(newfd.try_into().unwrap());
            }
            return Ok(());
        }

        if cmd == FD_CMD_DUP2 {
            if arg.is_null() || ret.is_null() {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            let to = unsafe { arg.cast::<RawFd>().read() };
            let Ok(to_idx) = usize::try_from(to) else {
                return Err(TwzError::INVALID_ARGUMENT);
            };
            if to_idx >= MAX_FD {
                return Err(TwzError::INVALID_ARGUMENT);
            }
            // Duplicating onto itself is a no-op, but still had to validate the source above.
            if to == fd {
                unsafe { ret.cast::<RawFd>().write(to) };
                return Ok(());
            }

            let file = file_desc
                .file
                .dup()
                .unwrap_or_else(|| file_desc.file.clone());
            let dup_file = file.clone();
            let mut nfd = file_desc.clone();
            nfd.file = file;
            nfd.cloexec = Arc::new(AtomicBool::new(false));
            let b = **nfd.binding;
            nfd.binding = MaybeNoDrop::new(Arc::new(b), true);

            // Whatever occupied the target descriptor is closed, as dup2 requires. Release the
            // slot lock before closing it, matching Self::close.
            let replaced = binding.insert(to_idx, nfd);
            drop(binding);
            if let Some(replaced) = replaced {
                // ...but only when it is not the very object we just duplicated. Fd::dup()
                // returns None for most kinds, so duplicates share a single Arc, and
                // Fd::close() shuts that shared object down for every descriptor referring to
                // it. Closing here would therefore break the canonical save-and-restore
                // sequence `saved = dup(1); ...; dup2(saved, 1)`, where the descriptor being
                // displaced is backed by the same object as the duplicate replacing it.
                if !Arc::ptr_eq(&replaced.file, &dup_file) {
                    let _ = replaced.file.close();
                }
            }
            unsafe { ret.cast::<RawFd>().write(to) };
            return Ok(());
        }

        file_desc.fd_cmd(cmd, arg, ret)
    }

    pub fn write(&self, fd: RawFd, buf: &[u8], ctx: *mut io_ctx) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        let len = file_desc
            .file
            .write(buf, file_desc.io_ctx_flags(ctx), None, None)?;
        Ok(len)
    }

    pub fn close(&self, fd: RawFd) -> Option<()> {
        let Some(file_desc) = get_fd_slots()
            .write()
            .unwrap()
            .remove(fd.try_into().unwrap())
        else {
            return Some(());
        };

        file_desc.file.close().ok()?;

        Some(())
    }

    pub fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize> {
        let binding = get_fd_slots().read().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        file_desc.seek(pos)
    }

    /// `Current` and `Root` are the naming server's to hold: both live on this compartment's
    /// handle there, so setting either is one gate call and *nothing* is recorded here. The rest
    /// (`Home`, `Temp`, `Exe`) are compartment-local conventions with no namespace behind them,
    /// and stay in the map.
    ///
    /// This used to call `set_naming_namespace` for **every** root, so setting `Home` moved the
    /// working directory -- and moved it without updating the `Current` entry, leaving the
    /// runtime reporting one directory while resolving relative paths in another.
    pub fn set_nameroot(&self, root: NameRoot, slice: &[u8]) -> Result<()> {
        let path = PathBuf::from(str::from_utf8(slice).map_err(|_| TwzError::INVALID_ARGUMENT)?);
        match root {
            NameRoot::Current => set_naming_namespace(&path),
            NameRoot::Root => {
                let handle = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
                handle.change_root(&path)?;
                // `/` moved, so what the working directory is called moved with it.
                cwd_memo_invalidate();
                Ok(())
            }
            // Nothing to set. `Home`, `Temp` and `Exe` are derived from where they actually
            // live (see `get_nameroot`) rather than stored, so there is no slot to write -- and
            // saying so is better than accepting a value that the next read would ignore.
            _ => Err(TwzError::NOT_SUPPORTED),
        }
    }

    pub fn fd_waitpoint(&self, fd: RawFd, kind: wait_kind) -> Result<(ThreadSyncSleep, bool)> {
        let binding = get_fd_slots().read().unwrap();
        let file_desc = binding
            .get(fd.try_into().unwrap())
            .cloned()
            .ok_or(ArgumentError::BadHandle)?;
        drop(binding);

        // This crosses the extern "C" twz_rt_fd_waitpoint ABI (see rt-abi's io.rs), which has
        // no way to carry an owning reference back to the caller, so `keepalive` is dropped
        // here rather than threaded through. This is sound only because the socket engine's
        // wait-word allocations are themselves permanently stable once created (see
        // engine.rs's Waiters::init_waiter) -- a raw pointer into one stays valid for the
        // life of the process even past handle reuse, independent of `keepalive`.
        let wp = file_desc.file.waitpoint(kind)?;
        Ok((wp.sleep, wp.ready))
    }

    pub fn get_nameroot(&self, root: NameRoot, slice: &mut [u8]) -> Result<usize> {
        /// A short write is how the caller is told to grow its buffer (see libstd's `read_name`),
        /// so report what was copied, never what was wanted.
        fn copy_out(data: &[u8], slice: &mut [u8]) -> usize {
            let len = data.len().min(slice.len());
            slice[0..len].copy_from_slice(&data[0..len]);
            len
        }
        match root {
            // Asked of the server, which is the only thing that knows: the working namespace is
            // per-handle state there and the path is derived from the namespace chain, so the
            // answer agrees with what a relative lookup -- and `..` -- will actually do.
            NameRoot::Current => Ok(copy_out(
                current_dir()?.as_os_str().as_encoded_bytes(),
                slice,
            )),
            // From inside, the root is `/` -- that is what a root is. *Which* namespace it names
            // is the server's business, on the handle.
            NameRoot::Root => Ok(copy_out(b"/", slice)),
            // Derived, not stored. Each of these has a real source, and a settable map of three
            // constants was standing in for all of them: the environment carries `HOME` and
            // `TMPDIR` the way it does everywhere else, and the executable is something the
            // monitor already knows about this compartment.
            NameRoot::Home => Ok(copy_out(
                std::env::var("HOME").as_deref().unwrap_or("/").as_bytes(),
                slice,
            )),
            NameRoot::Temp => Ok(copy_out(
                std::env::var("TMPDIR")
                    .as_deref()
                    .unwrap_or("/tmp")
                    .as_bytes(),
                slice,
            )),
            // TODO: this should come from the compartment config -- the monitor knows which
            // program it loaded -- rather than being a placeholder that reports the root.
            NameRoot::Exe => Ok(copy_out(b"/", slice)),
        }
    }

    pub fn fd_enumerate(
        &self,
        fd: RawFd,
        buf: &mut [twizzler_rt_abi::fd::NameEntry],
        off: usize,
    ) -> Result<usize> {
        tracing::trace!(
            "fd_enumerate: fd={}, off={} ({}), buf_len={}",
            fd,
            off,
            off * size_of::<twizzler_rt_abi::fd::NameEntry>(),
            buf.len()
        );
        let stat = self.fd_get_info(fd).ok_or(ArgumentError::BadHandle)?;
        let t_acq = std::time::Instant::now();
        let session = get_naming_handle().ok_or(TwzError::NOT_SUPPORTED)?;
        let acq_ns = t_acq.elapsed().as_nanos() as u64;
        let t_gate = std::time::Instant::now();
        let names = session.enumerate_names_nsid(stat.id.into(), off, buf.len())?;
        let gate_ns = t_gate.elapsed().as_nanos() as u64;
        let t_conv = std::time::Instant::now();
        tracing::trace!("enumerate_names_nsid returned {} entries", names.len());
        let end = buf.len().min(names.len());
        for i in 0..end {
            let name = &names[i];
            let Ok(entry_name) = name.name() else {
                // The index is the caller's cursor -- it advances `off` by the count returned, so
                // compacting past a bad name would slide every later entry down and desynchronize
                // the next chunk. Write an empty name instead: `continue` alone left this slot
                // holding whatever the previous chunk put there, which a caller reusing its buffer
                // (libstd's `ReadDir` does) reads back as a duplicate of an unrelated entry.
                buf[i] = twizzler_rt_abi::fd::NameEntry::default();
                continue;
            };
            let ne = if name.kind == NsNodeKind::SymLink {
                twizzler_rt_abi::fd::NameEntry::new_symlink(
                    entry_name.as_bytes(),
                    name.readlink()?.as_bytes(),
                    twizzler_rt_abi::fd::FdInfo {
                        kind: match name.kind {
                            naming_core::NsNodeKind::Namespace => {
                                twizzler_rt_abi::fd::FdKind::Directory
                            }
                            naming_core::NsNodeKind::Object => twizzler_rt_abi::fd::FdKind::Regular,
                            naming_core::NsNodeKind::SymLink => {
                                twizzler_rt_abi::fd::FdKind::SymLink
                            }
                        },
                        flags: twizzler_rt_abi::fd::FdFlags::empty(),
                        id: name.id.raw(),
                        size: 0,
                        unix_mode: 0,
                        accessed: std::time::Duration::ZERO,
                        modified: std::time::Duration::ZERO,
                        created: std::time::Duration::ZERO,
                    }
                    .into(),
                )
            } else {
                twizzler_rt_abi::fd::NameEntry::new(
                    entry_name.as_bytes(),
                    twizzler_rt_abi::fd::FdInfo {
                        kind: match name.kind {
                            naming_core::NsNodeKind::Namespace => {
                                twizzler_rt_abi::fd::FdKind::Directory
                            }
                            naming_core::NsNodeKind::Object => twizzler_rt_abi::fd::FdKind::Regular,
                            naming_core::NsNodeKind::SymLink => {
                                twizzler_rt_abi::fd::FdKind::SymLink
                            }
                        },
                        flags: twizzler_rt_abi::fd::FdFlags::empty(),
                        id: name.id.raw(),
                        size: 0,
                        unix_mode: 0,
                        accessed: std::time::Duration::ZERO,
                        modified: std::time::Duration::ZERO,
                        created: std::time::Duration::ZERO,
                    }
                    .into(),
                )
            };
            buf[i] = ne;
        }
        enumstats::record(
            acq_ns,
            gate_ns,
            t_conv.elapsed().as_nanos() as u64,
            end as u64,
        );
        Ok(end)
    }
}

// Temporary instrumentation for the directory-enumeration latency hunt (pagerperf.md).
mod enumstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static ENTRIES: AtomicU64 = AtomicU64::new(0);
    static ACQ: AtomicU64 = AtomicU64::new(0);
    static GATE: AtomicU64 = AtomicU64::new(0);
    static CONV: AtomicU64 = AtomicU64::new(0);

    pub fn record(acq: u64, gate: u64, conv: u64, entries: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let a = ACQ.fetch_add(acq, Ordering::Relaxed) + acq;
        let g = GATE.fetch_add(gate, Ordering::Relaxed) + gate;
        let c = CONV.fetch_add(conv, Ordering::Relaxed) + conv;
        let e = ENTRIES.fetch_add(entries, Ordering::Relaxed) + entries;
        if secgate::statcadence::report_now(n) {
            secgate::statline!(
                "ENUMSTATS {} calls, {} entries: acquire {} us, gate {} us, convert {} us",
                n,
                e,
                a / 1000,
                g / 1000,
                c / 1000,
            );
        }
    }
}
