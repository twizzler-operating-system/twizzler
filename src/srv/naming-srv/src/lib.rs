#![feature(linkage)]
#![feature(io_error_more)]
#![feature(thread_local)]
#[warn(unused_variables)]
use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};

use lazy_init::LazyTransform;
use lazy_static::lazy_static;
use naming_core::{
    CwdPath, GetFlags, InlinePath, NameSession, NameStore, NsNode, Result, BUFFER_SLOT_SIZE,
    PATH_MAX,
};
use secgate::util::{Descriptor, HandleMgr, SimpleBuffer};
use tracing::Level;
use twizzler::{error::SecurityError, object::ObjectHandle};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_get_random, sys_object_create, BackingType, CreateTieFlags, CreateTieSpec,
        GetRandomFlags, LifetimeType, ObjectCreate, ObjectCreateFlags,
    },
};
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError},
    object::{MapFlags, ObjID},
};

struct SbObjects {
    objs: Vec<ObjectHandle>,
}

static SB_OBJECTS: Mutex<SbObjects> = Mutex::new(SbObjects { objs: Vec::new() });

pub fn get_sb_object(instance: ObjID) -> Result<ObjectHandle> {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    if sbo.objs.len() == 0 {
        drop(sbo);
        // Create and map a handle for the simple buffer.
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::DELETE,
                Protections::all(),
            ),
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty()).into()],
        )?;
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, MapFlags::WRITE | MapFlags::READ)?;
        return Ok(handle);
    }

    // Wiped by `NamespaceClient::into_handle` on release, so a pooled object carries nothing from
    // its previous holder.
    Ok(sbo.objs.pop().unwrap())
}

pub fn release_sb_object(obj: ObjectHandle) {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    sbo.objs.push(obj);
}

/// One open naming handle -- one per client runtime, shared by all its threads.
///
/// Nothing here serializes two calls on the same descriptor: each call clones a [`NameSession`]
/// out of `session` (a store reference plus an `Arc`) and runs against the store's own interior
/// locks. Paths arrive either inline in the gate arguments or in caller-chosen disjoint slots of
/// the shared buffer, so concurrent calls never alias buffer ranges unless the client races
/// itself -- in which case it corrupts only its own results.
struct NamespaceClient<'a> {
    instance: ObjID,
    /// This client's authoritative naming state: its root and its working namespace. The
    /// runtime on the other side of the gate keeps no copy of either -- it asks. Snapshot-cloned
    /// per call for reads; write-locked in place by `change_namespace`/`change_root`.
    session: RwLock<NameSession<'a>>,
    /// Created on demand: a client that only does inline calls never needs one, and making every
    /// handle carry one is what made handles expensive.
    buffer: OnceLock<SimpleBuffer>,
    buffer_init: Mutex<()>,
    /// High-water mark of buffer bytes this client ever used, so a recycled buffer is wiped over
    /// exactly that much rather than all `max_len()` (a gigabyte) of it.
    used: AtomicUsize,
}

// Safety: same assertion `NameStore` already makes for itself; shared access to the session
// template is mediated by the RwLock.
unsafe impl Send for NamespaceClient<'_> {}
unsafe impl Sync for NamespaceClient<'_> {}

impl<'a> NamespaceClient<'a> {
    fn new(session: NameSession<'a>, instance: ObjID) -> Self {
        Self {
            instance,
            session: RwLock::new(session),
            buffer: OnceLock::new(),
            buffer_init: Mutex::new(()),
            used: AtomicUsize::new(0),
        }
    }

    /// A session for one call: the current working namespace, snapshot at call entry.
    fn session(&self) -> NameSession<'a> {
        self.session.read().unwrap().clone()
    }

    fn note_used(&self, end: usize) {
        self.used.fetch_max(end, Ordering::Relaxed);
    }

    /// Create the shared buffer if this is the first call that needs one, and report its object.
    fn buffer_id(&self) -> Result<ObjID> {
        if let Some(buffer) = self.buffer.get() {
            return Ok(buffer.handle().id());
        }
        let _init = self.buffer_init.lock().unwrap();
        if self.buffer.get().is_none() {
            let _ = self
                .buffer
                .set(SimpleBuffer::new(get_sb_object(self.instance)?));
        }
        // Unwrap-Ok: set above under the init lock.
        Ok(self.buffer.get().unwrap().handle().id())
    }

    fn buffer(&self) -> Result<&SimpleBuffer> {
        self.buffer.get().ok_or(ArgumentError::BadHandle.into())
    }

    /// Read a caller-supplied path out of the shared buffer, bounds-checked. Slot discipline is
    /// the client's own problem; the server only guarantees it never reads outside the buffer.
    fn read_path(&self, offset: usize, name_len: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let buffer = self.buffer()?;
        let end = offset
            .checked_add(name_len)
            .ok_or(ArgumentError::InvalidArgument)?;
        if end > buffer.max_len() {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.note_used(end);
        let mut buf = vec![0; name_len];
        buffer.read_offset(&mut buf, offset);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ErrorKind::InvalidFilename)?,
        ))
    }

    /// Zero what this client put in the buffer and recover the object, before it goes back in the
    /// pool for a different security context to be handed.
    ///
    /// Objects are zero-filled at creation, so only a recycled buffer needs the wipe. Discarding
    /// the object's pages would be cheaper, but `MapControlCmd::Discard` only zeroes a mapping
    /// that has a stable page table behind it, which a volatile buffer object does not.
    fn into_handle(self) -> Option<ObjectHandle> {
        let used = self.used.load(Ordering::Relaxed);
        let buffer = self.buffer.into_inner()?;
        const CHUNK: usize = 4096;
        let zeros = [0u8; CHUNK];
        let mut off = 0;
        while off < used {
            let n = buffer.write_offset(&zeros[..CHUNK.min(used - off)], off);
            if n == 0 {
                break;
            }
            off += n;
        }
        Some(buffer.into_handle())
    }
}

/// A root and working namespace held for a compartment that has not started yet.
///
/// The chain each namespace carries -- the name it was opened under in its parent, all the way up
/// -- is live server state. That is precisely what a path or an ObjID in the child's config could
/// not carry: a path loses identity across a rename, an ObjID loses the chain and would leave the
/// child's `getcwd` reporting `/`. So the state stays here and the child collects it.
struct Bequest {
    session: NameSession<'static>,
    issued: Instant,
}

// Safety: the same assertion `NamespaceClient` makes for the session it holds; a bequest is a
// session snapshot and is only ever reached under `BEQUESTS`.
unsafe impl Send for Bequest {}

/// An uncollected bequest pins its namespaces, so it expires. A compartment that starts later
/// than this simply begins at its root.
const BEQUEST_TTL: Duration = Duration::from_secs(60);
/// Bound on outstanding bequests, so a compartment that mints without ever spawning cannot grow
/// the table without limit.
const MAX_BEQUESTS: usize = 64;

static BEQUESTS: Mutex<BTreeMap<u64, Bequest>> = Mutex::new(BTreeMap::new());

/// Look up a client and take a reference to it, releasing the handle-table lock before the caller
/// does any work. The table is read-locked per call and write-locked only by open/close, so calls
/// neither serialize on the table nor on each other.
fn lookup_client(comp: ObjID, desc: Descriptor) -> Option<Arc<NamespaceClient<'static>>> {
    let service = NAMINGSERVICE.get()?;
    let binding = service.handles.read().unwrap();
    binding.lookup(comp, desc).cloned()
}

unsafe impl Send for Namer<'_> {}
unsafe impl Sync for Namer<'_> {}

struct Namer<'a> {
    handles: RwLock<HandleMgr<Arc<NamespaceClient<'a>>>>,
    names: NameStore,
}

impl Namer<'_> {
    fn new() -> Self {
        Self {
            handles: RwLock::new(HandleMgr::new(None)),
            names: NameStore::new(),
        }
    }

    fn new_with(id: ObjID) -> Result<Self> {
        let names = NameStore::new_with(id)?;
        Ok(Self {
            handles: RwLock::new(HandleMgr::new(None)),
            names,
        })
    }
}

lazy_static! {
    static ref NAMINGSERVICE: LazyTransform<(), Namer<'static>> = LazyTransform::new(());
}

fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

// How would this work if I changed the root while handles were open?
#[secgate::entry(lib = "naming-core")]
pub fn namer_start(bootstrap: ObjID) -> Result<ObjID> {
    // Anyone can call this gate; a second call must not unwind out of an extern "C" entry.
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    );

    // Build identity, not just configuration: a sweep can be handed an image built from another
    // session's source with the command line it asked for, and nothing else in the transcript
    // distinguishes that from its own binaries. Behind the gate now — a sweep auditing arms must
    // pass `--diag=naming` to get the memo line.
    if diag_enabled() {
        twizzler_abi::klog_println!("NAMEMEMO {}", naming_core::memo_config());
    }

    Ok(NAMINGSERVICE
        .get_or_create(|_| {
            let namer = Namer::new_with(bootstrap)
                .or::<ErrorKind>(Ok(Namer::new()))
                .unwrap();
            namer.names.root_session().mkns("/initrd", false).unwrap();
            for n in get_kernel_init_info().names() {
                if n.name() != ".." && n.name() != "." {
                    // A name the store rejects (too long, say) costs us that one entry, not boot.
                    let _ = namer
                        .names
                        .root_session()
                        .put(&format!("/initrd/{}", n.name()), n.id())
                        .inspect_err(|e| tracing::warn!("failed to bind initrd name: {}", e));
                }
            }

            namer
        })
        .names
        .id())
}

#[secgate::entry(lib = "naming-core")]
pub fn open_handle() -> Result<Descriptor> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let service = NAMINGSERVICE.get().ok_or(ResourceError::Unavailable)?;
    let mut binding = service.handles.write().unwrap();

    let session = service.names.root_session();
    let client = NamespaceClient::new(session, info.source_context().unwrap_or(0.into()));

    let r = binding
        .insert(info.source_context().unwrap_or(0.into()), Arc::new(client))
        .ok_or(ResourceError::OutOfResources.into());

    // DIAG: is `HandleMgr::gc_handles` actually reclaiming dead compartments' tables?
    //
    // It tries to -- `retain(|id, sv| !sv.is_empty() && sys_object_stat(*id).is_ok())` -- but that
    // predicate is only as good as a dead compartment's security-context object ceasing to stat,
    // and measured from the client side it keeps returning `Ok` for roughly half of exited
    // children. A total that climbs with spawns is that failure made visible from the service side;
    // one that stays flat means the retention is somewhere other than the handle table, and this
    // line says which without anyone having to infer it. Every 32nd open, so a spawn loop reports
    // without the log becoming the cost.
    static OPENS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let n = OPENS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // Caller-thread identity. Settled: `threads == opens + 1` at all 42 samples of
    // many-d3-naming6, so gate calls really do each arrive on a distinct thread -- but ferroc's
    // base chunks are flat across the same run, which kills the "entering threads accumulate
    // ferroc chunks" chain at its last link regardless. The set that settled it grew one u128 per
    // gate call and was never trimmed, making it 89.6% of the residual it was measuring. Constant
    // space now: only a repeated id would falsify the premise, and catching a repeat costs a word.
    static PREV_TID: Mutex<u128> = Mutex::new(0);
    static TID_REPEATS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let tid_repeats = {
        let tid = info.thread_id().raw();
        let mut g = PREV_TID.lock().unwrap();
        let rep = if *g == tid {
            TID_REPEATS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1
        } else {
            TID_REPEATS.load(core::sync::atomic::Ordering::Relaxed)
        };
        *g = tid;
        rep
    };
    if n % 32 == 0 && diag_enabled() {
        let (ns, names, order, pinned) = naming_core::cache_stats();
        heap_objects_line();
        base_chunk_line();
        twizzler_abi::klog_println!(
            "NAMING-HANDLES opens={} tid_repeats={} live_total={} compartments={} ns_cached={} ns_pinned={} names={} order={}",
            n,
            tid_repeats,
            binding.total_count(),
            binding.handles().map(|h| h.0).collect::<std::collections::BTreeSet<_>>().len(),
            ns,
            pinned,
            names,
            order,
        );
    }
    r
}

#[secgate::entry(lib = "naming-core")]
pub fn bequeath(desc: Descriptor) -> Result<u64> {
    let client = client_for(desc)?;
    let session = client.session();

    // Unguessable: possession of the token is what authorises collecting the bequest, and the
    // token reaches the child through its compartment config, which only it can read.
    let mut raw = [core::mem::MaybeUninit::<u8>::uninit(); 8];
    let n = sys_get_random(&mut raw, GetRandomFlags::empty())?;
    if n != raw.len() {
        return Err(ResourceError::Unavailable.into());
    }
    // Safety: `sys_get_random` reported all 8 bytes written.
    let token = u64::from_le_bytes(unsafe { core::mem::transmute::<_, [u8; 8]>(raw) });
    // Zero is "no bequest" in the compartment config, so it cannot name one.
    if token == 0 {
        return Err(ResourceError::Unavailable.into());
    }

    let mut table = BEQUESTS.lock().unwrap();
    let now = Instant::now();
    table.retain(|_, b| now.duration_since(b.issued) < BEQUEST_TTL);
    if table.len() >= MAX_BEQUESTS {
        // Drop the oldest rather than refusing to mint: a bequest still outstanding after 63
        // others is one nobody is coming for, and refusing would break the live spawn instead.
        if let Some(oldest) = table.iter().min_by_key(|(_, b)| b.issued).map(|(k, _)| *k) {
            table.remove(&oldest);
        }
    }
    table.insert(
        token,
        Bequest {
            session,
            issued: now,
        },
    );
    Ok(token)
}

#[secgate::entry(lib = "naming-core")]
pub fn redeem_bequest(desc: Descriptor, token: u64) -> Result<()> {
    let client = client_for(desc)?;
    let bequest = BEQUESTS.lock().unwrap().remove(&token);
    match bequest {
        Some(bequest) => {
            *client.session.write().unwrap() = bequest.session;
            Ok(())
        }
        // Expired, already collected, or never existed. Not an error: the handle stays at its
        // root, which is where a compartment without a bequest starts anyway.
        None => {
            tracing::debug!("no bequest for token {:#x}", token);
            Ok(())
        }
    }
}

#[secgate::entry(lib = "naming-core")]
pub fn get_buffer(desc: Descriptor) -> Result<ObjID> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client = lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;
    client.buffer_id()
}

#[secgate::entry(lib = "naming-core")]
pub fn close_handle(desc: Descriptor) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let service = NAMINGSERVICE.get().unwrap();

    let mut binding = service.handles.write().unwrap();

    if let Some(client) = binding.remove(info.source_context().unwrap_or(0.into()), desc) {
        drop(binding);
        // Recycle the buffer object only if nobody is mid-call on this descriptor. If someone is,
        // the object is freed when their reference drops -- it just doesn't rejoin the pool.
        match Arc::try_unwrap(client) {
            Ok(client) => {
                if let Some(handle) = client.into_handle() {
                    release_sb_object(handle);
                }
            }
            Err(_) => tracing::warn!("closed descriptor {} while a call was in flight", desc),
        }
    }

    Ok(())
}

/// Client lookup shared by every operation entry.
fn client_for(desc: Descriptor) -> Result<Arc<NamespaceClient<'static>>> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle.into())
}

// ---- inline forms: the path never touches shared state -----------------------------------------

#[secgate::entry(lib = "naming-core")]
pub fn put_inline(desc: Descriptor, path: InlinePath, id: ObjID) -> Result<()> {
    client_for(desc)?.session().put(path.as_str()?, id)
}

#[secgate::entry(lib = "naming-core")]
pub fn mkns_inline(desc: Descriptor, path: InlinePath, persist: bool) -> Result<()> {
    client_for(desc)?.session().mkns(path.as_str()?, persist)
}

#[secgate::entry(lib = "naming-core")]
pub fn link_inline(desc: Descriptor, path: InlinePath, link: InlinePath) -> Result<()> {
    client_for(desc)?
        .session()
        .link(path.as_str()?, link.as_str()?)
}

#[secgate::entry(lib = "naming-core")]
pub fn get_inline(desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
    let client = client_for(desc)?;
    let session = client.session();

    // `as_str`, not `to_path`: the path is already sitting in the gate's arguments and
    // `NameSession::get` wants a `&str` back out of whatever it is handed, so the `PathBuf` was an
    // allocate-and-free per lookup purely to change type. Kept because it is less work, not because
    // it is faster -- A/B'd at four rounds a side and the effect is below this instrument's
    // resolution (22 in this file).
    session.get(path.as_str()?, flags)
}

#[secgate::entry(lib = "naming-core")]
pub fn remove_inline(desc: Descriptor, path: InlinePath) -> Result<()> {
    client_for(desc)?.session().remove(path.as_str()?)
}

#[secgate::entry(lib = "naming-core")]
pub fn rename_inline(desc: Descriptor, old: InlinePath, new: InlinePath) -> Result<()> {
    client_for(desc)?
        .session()
        .rename(old.as_str()?, new.as_str()?)
}

/// Moves are made *in place* under the write lock rather than snapshot-mutate-write-back: root
/// and working namespace now live in the same session, so a concurrent `change_root` and
/// `change_namespace` would otherwise each write back a session missing the other's change. The
/// walk runs under the lock, which briefly blocks this handle's other calls; a chdir is rare and
/// a lost one is not.
#[secgate::entry(lib = "naming-core")]
pub fn change_namespace_inline(desc: Descriptor, path: InlinePath) -> Result<()> {
    let client = client_for(desc)?;
    let mut session = client.session.write().unwrap();
    session.change_namespace(path.as_str()?)
}

#[secgate::entry(lib = "naming-core")]
pub fn change_root_inline(desc: Descriptor, path: InlinePath) -> Result<()> {
    let client = client_for(desc)?;
    let mut session = client.session.write().unwrap();
    session.change_root(path.as_str()?)
}

/// The caller's working directory, derived from the namespace chain on every call.
#[secgate::entry(lib = "naming-core")]
pub fn get_cwd_inline(desc: Descriptor) -> Result<CwdPath> {
    let client = client_for(desc)?;
    let path = client.session().cwd_path()?;
    Ok(CwdPath::new(path))
}

/// Spill form of [`get_cwd_inline`]: writes the path at `offset` and reports its length.
#[secgate::entry(lib = "naming-core")]
pub fn get_cwd(desc: Descriptor, offset: usize, cap: usize) -> Result<usize> {
    let client = client_for(desc)?;
    let path = client.session().cwd_path()?;
    let bytes = path.as_os_str().as_encoded_bytes();
    let buffer = client.buffer()?;
    let end = offset
        .checked_add(bytes.len())
        .ok_or(ArgumentError::InvalidArgument)?;
    if bytes.len() > cap || end > buffer.max_len() {
        return Err(ArgumentError::InvalidArgument.into());
    }
    let n = buffer.write_offset(bytes, offset);
    client.note_used(offset + n);
    Ok(n)
}

// ---- buffer (spill) forms: paths live at caller-chosen slot offsets ----------------------------

#[secgate::entry(lib = "naming-core")]
pub fn put(desc: Descriptor, offset: usize, name_len: usize, id: ObjID) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    client.session().put(path, id)
}

#[secgate::entry(lib = "naming-core")]
pub fn mkns(desc: Descriptor, offset: usize, name_len: usize, persist: bool) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    client.session().mkns(path, persist)
}

#[secgate::entry(lib = "naming-core")]
pub fn link(desc: Descriptor, offset: usize, name_len: usize, link_len: usize) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    let link = client.read_path(offset + name_len, link_len)?;
    client.session().link(path, link)
}

#[secgate::entry(lib = "naming-core")]
pub fn get(desc: Descriptor, offset: usize, name_len: usize, flags: GetFlags) -> Result<NsNode> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    client.session().get(path, flags)
}

#[secgate::entry(lib = "naming-core")]
pub fn remove(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    client.session().remove(path)?;
    Ok(())
}

#[secgate::entry(lib = "naming-core")]
pub fn rename(desc: Descriptor, offset: usize, old_len: usize, new_len: usize) -> Result<()> {
    let client = client_for(desc)?;
    let old_path = client.read_path(offset, old_len)?;
    let new_path = client.read_path(offset + old_len, new_len)?;
    client.session().rename(old_path, new_path)
}

#[secgate::entry(lib = "naming-core")]
pub fn change_namespace(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    let mut session = client.session.write().unwrap();
    session.change_namespace(path)
}

#[secgate::entry(lib = "naming-core")]
pub fn change_root(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    let mut session = client.session.write().unwrap();
    session.change_root(path)
}

// ---- enumeration: the reply is written back into the caller's slot -----------------------------

/// Write `nodes` into the client's buffer at `offset` and report how many fit. The count was
/// already clamped to what a slot holds, so a short write only happens for a hostile offset near
/// the end of the buffer -- report what was actually written, never more.
fn write_enumeration(
    client: &NamespaceClient<'_>,
    offset: usize,
    nodes: &[NsNode],
) -> Result<usize> {
    let slice =
        unsafe { std::slice::from_raw_parts(nodes.as_ptr() as *const u8, size_of_val(nodes)) };
    let n = client.buffer()?.write_offset(slice, offset);
    client.note_used(offset + n);
    Ok(n / size_of::<NsNode>())
}

fn slot_entry_cap(count: usize) -> usize {
    count.min(BUFFER_SLOT_SIZE / size_of::<NsNode>())
}

#[secgate::entry(lib = "naming-core")]
pub fn enumerate_names(
    desc: Descriptor,
    offset: usize,
    name_len: usize,
    skip: usize,
    count: usize,
) -> Result<usize> {
    let client = client_for(desc)?;
    let path = client.read_path(offset, name_len)?;
    let nodes = client
        .session()
        .enumerate_namespace(path, skip, slot_entry_cap(count))?;
    write_enumeration(&client, offset, &nodes)
}

#[secgate::entry(lib = "naming-core")]
pub fn enumerate_names_nsid(
    desc: Descriptor,
    id: ObjID,
    offset: usize,
    skip: usize,
    count: usize,
) -> Result<usize> {
    let client = client_for(desc)?;
    let nodes = client
        .session()
        .enumerate_namespace_nsid(id, skip, slot_entry_cap(count))?;
    write_enumeration(&client, offset, &nodes)
}

// ---- naming-srv's own heap census --------------------------------------------------------------
//
// `leakcheck` proved this service's heap grows ~134 KB per compartment load while the *spawner's*
// heap is flat, its handle table is flat, and its namespace cache is bit-identical across 224
// opens. Three mechanisms proposed, three measured and dead. The remaining question is what is
// actually being retained, and the instrument that answers it already exists -- the per-size-class
// census in `twz-rt` -- it has simply only ever been armed inside leakcheck. The counters are
// per-compartment statics, so arming it here counts *this* compartment's allocations and nobody
// else's.
//
// Reported as a delta against the previous report, not a running total: a cumulative figure read
// every 32 opens attributes all of history to the latest window, which is the same mistake as
// reading a boot-long counter per op.

/// Whether the `naming` diagnostic class was requested via `TWZ_DIAG` (comma list, or `all`).
/// Same contract as `twizzler_net::diag_enabled`, which this crate does not depend on; init logs
/// the value at boot, so a log without NAMING-* lines provably means "off".
fn diag_enabled() -> bool {
    static SET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let set = SET.get_or_init(|| std::env::var("TWZ_DIAG").unwrap_or_default());
    set.split(',').any(|c| c == "naming" || c == "all")
}

unsafe extern "C" {
    fn __twz_rt_diag_heap_objects(out: *mut u64, n: usize) -> usize;
    fn __twz_rt_diag_decommit_stats(out: *mut u64);
    fn __twz_rt_diag_cross_threads() -> u64;
}

/// ferroc's base-chunk traffic for this compartment.
///
/// The class census counts what goes through `GlobalAlloc`; it does **not** count what ferroc takes
/// from talc underneath, because those requests go straight to `LOCAL_ALLOCATOR.alloc`. So a
/// compartment whose live-block set is flat can still grow its heap object by whole slabs, and the
/// census would show nothing -- which is exactly the shape of what is left unexplained here:
/// naming's main heap object gains ~100 KB per compartment load while the census can account for
/// only ~237 KB of retention across the entire run.
///
/// `hook_dealloc` reading 0 means ferroc never hands a chunk back, so `base_alloc` bytes are a
/// high-water mark that only rises.
fn base_chunk_line() {
    let mut d = [0u64; 8];
    unsafe { __twz_rt_diag_decommit_stats(d.as_mut_ptr()) };
    twizzler_abi::klog_println!(
        "NAMING-BASECHUNK alloc={}/{} dealloc_bytes={} hook_decommit={} hook_dealloc={} cross_threads_held={}",
        d[5], d[6], d[7], d[0], d[1],
        unsafe { __twz_rt_diag_cross_threads() }
    );
}

/// Which heap objects this compartment's allocator actually owns.
///
/// **This is a check on the attribution, not on the leak.** Growers are currently assigned an owner
/// by an object note of the form `heap:<sctx>`, written from the allocator's OOM handler using
/// `get_sctx_id()` -- and during a cross-compartment gate call the *active* security context is the
/// callee's, not the caller's. So an object created by compartment A while A happens to be
/// executing inside B's gate would be labelled B. Every "this grower belongs to naming" conclusion
/// rests on that note.
///
/// `oom_handler.objects` is the definitive list: `create_and_map` is only ever called from talc's
/// OOM handler, so an id here is this compartment's and an id absent from here is not. If the
/// rotating ~24 MB growers are absent from this list, the note is mislabelling them and the
/// remaining ~100 KB/spawn is not naming's at all.
fn heap_objects_line() {
    let mut buf = [0u64; 3 * 64 + 2];
    let n = unsafe { __twz_rt_diag_heap_objects(buf.as_mut_ptr(), buf.len()) };
    if n < 2 {
        twizzler_abi::klog_println!("NAMING-HEAPOBJ unavailable");
        return;
    }
    twizzler_abi::klog_println!("NAMING-HEAPOBJ main={} early={}", buf[n - 2], buf[n - 1]);
    for i in (0..n - 2).step_by(3) {
        twizzler_abi::klog_println!(
            "NAMING-HEAPOBJ-ID slot={} id={:x}{:016x} kind={}",
            buf[i],
            buf[i + 1],
            buf[i + 2],
            if (i / 3) < buf[n - 2] as usize {
                "main"
            } else {
                "early"
            }
        );
    }
}
