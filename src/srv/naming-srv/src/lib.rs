#![feature(linkage)]
#![feature(io_error_more)]
#![feature(thread_local)]
#[warn(unused_variables)]
use std::sync::{Arc, Mutex};
use std::{io::ErrorKind, path::PathBuf};

use lazy_init::LazyTransform;
use lazy_static::lazy_static;
use naming_core::{GetFlags, InlinePath, NameSession, NameStore, NsNode, Result, PATH_MAX};
use secgate::{
    util::{Descriptor, HandleMgr, SimpleBuffer},
    TwzError,
};
use tracing::Level;
use twizzler::{error::SecurityError, object::ObjectHandle};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
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

    // Wiped by `ClientInner::wipe` on release, so a pooled object carries nothing from its
    // previous holder.
    Ok(sbo.objs.pop().unwrap())
}

pub fn release_sb_object(obj: ObjectHandle) {
    let mut sbo = SB_OBJECTS.lock().unwrap();
    sbo.objs.push(obj);
}

/// A client's session and its shared buffer, reachable only under [`NamespaceClient::inner`].
///
/// The buffer is created on demand: a client that only does short-path lookups never needs one, and
/// making every handle carry one is what made a pool of handles expensive.
struct ClientInner<'a> {
    session: NameSession<'a>,
    buffer: Option<SimpleBuffer>,
    /// How far into the buffer this client has ever had data, so a recycled buffer can be wiped
    /// over exactly that much of it rather than all `max_len()` (a gigabyte) of it.
    used: usize,
}

impl ClientInner<'_> {
    fn buffer(&self) -> Result<&SimpleBuffer> {
        self.buffer.as_ref().ok_or(ArgumentError::BadHandle.into())
    }

    fn note_used(&mut self, end: usize) {
        self.used = self.used.max(end);
    }

    /// Zero what this client put in the buffer, before the object goes back in the pool for a
    /// different security context to be handed.
    ///
    /// Objects are zero-filled at creation, so only a recycled buffer needs this. Discarding the
    /// object's pages would be cheaper, but `MapControlCmd::Discard` only zeroes a mapping that
    /// has a stable page table behind it, which a volatile buffer object does not.
    fn wipe(&mut self) {
        let used = self.used;
        let Some(buffer) = self.buffer.as_mut() else {
            return;
        };
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
        self.used = 0;
    }

    /// Write through the handle's own buffer rather than building a fresh [`SimpleBuffer`] over a
    /// clone of its handle: `SimpleBuffer::new` formats a string and adds an object note, so a
    /// throwaway one costs a syscall per call and leaves a note behind that nothing removes.
    fn buffer_mut(&mut self) -> Result<&mut SimpleBuffer> {
        self.buffer.as_mut().ok_or(ArgumentError::BadHandle.into())
    }
}

/// One open naming handle.
///
/// The lock is per-client, not per-server: two descriptors can be in `namei` at once, which is the
/// point (`NameStore` is declared `Send + Sync` and every namespace it walks has its own interior
/// lock). What it still serializes is two calls arriving on the *same* descriptor, which share one
/// `SimpleBuffer` -- a caller that raced its own handle would otherwise have one call read the
/// other's path and return the wrong object.
struct NamespaceClient<'a> {
    instance: ObjID,
    inner: Mutex<ClientInner<'a>>,
}

// Safety: same assertion `NameStore` already makes for itself. Everything non-Send here is reached
// only through `inner`'s mutex.
unsafe impl Send for NamespaceClient<'_> {}
unsafe impl Sync for NamespaceClient<'_> {}

impl<'a> NamespaceClient<'a> {
    fn new(session: NameSession<'a>, instance: ObjID) -> Option<Self> {
        Some(Self {
            instance,
            inner: Mutex::new(ClientInner {
                session,
                buffer: None,
                used: 0,
            }),
        })
    }

    /// Create the shared buffer if this is the first call that needs one, and report its object.
    fn buffer_id(&self) -> Result<ObjID> {
        let mut inner = self.inner.lock().unwrap();
        if inner.buffer.is_none() {
            inner.buffer = Some(SimpleBuffer::new(get_sb_object(self.instance)?));
        }
        // Unwrap-Ok: just filled in above.
        Ok(inner.buffer.as_ref().unwrap().handle().id())
    }

    fn into_handle(self) -> Option<ObjectHandle> {
        let mut inner = self.inner.into_inner().unwrap();
        inner.wipe();
        Some(inner.buffer?.into_handle())
    }
}

/// Look up a client and take a reference to it, releasing the handle-table lock before the caller
/// does any work. Holding that lock across the operation is what made every naming call in the
/// system serialize against every other one.
fn lookup_client(comp: ObjID, desc: Descriptor) -> Option<Arc<NamespaceClient<'static>>> {
    let service = NAMINGSERVICE.get()?;
    let binding = service.handles.lock().unwrap();
    binding.lookup(comp, desc).cloned()
}

impl<'a> ClientInner<'a> {
    fn read_buffer(&mut self, name_len: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.note_used(name_len);
        let mut buf = vec![0; name_len];
        self.buffer()?.read(&mut buf);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ErrorKind::InvalidFilename)?,
        ))
    }

    fn read_buffer_at(&mut self, name_len: usize, off: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.note_used(off.saturating_add(name_len));
        let mut buf = vec![0; name_len];
        self.buffer()?.read_offset(&mut buf, off);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ArgumentError::InvalidArgument)?,
        ))
    }
}

unsafe impl Send for Namer<'_> {}
unsafe impl Sync for Namer<'_> {}

struct Namer<'a> {
    handles: Mutex<HandleMgr<Arc<NamespaceClient<'a>>>>,
    names: NameStore,
}

impl Namer<'_> {
    fn new() -> Self {
        Self {
            handles: Mutex::new(HandleMgr::new(None)),
            names: NameStore::new(),
        }
    }

    fn new_with(id: ObjID) -> Result<Self> {
        let names = NameStore::new_with(id)?;
        Ok(Self {
            handles: Mutex::new(HandleMgr::new(None)),
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
#[secgate::entry(lib = "naming")]
pub fn namer_start(bootstrap: ObjID) -> Result<ObjID> {
    heap_census_arm();
    // Anyone can call this gate; a second call must not unwind out of an extern "C" entry.
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    );

    // Build identity, not just configuration: a sweep can be handed an image built from another
    // session's source with the command line it asked for, and nothing else in the transcript
    // distinguishes that from its own binaries.
    twizzler_abi::klog_println!("NAMEMEMO {}", naming_core::memo_config());

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

#[secgate::entry(lib = "naming")]
pub fn open_handle() -> Result<Descriptor> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let service = NAMINGSERVICE.get().ok_or(ResourceError::Unavailable)?;
    let mut binding = service.handles.lock().unwrap();

    let session = service.names.root_session();
    let client = NamespaceClient::new(session, info.source_context().unwrap())
        .ok_or(ResourceError::Unavailable)?;

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
    if n % 32 == 0 {
        let (ns, names, order, pinned) = naming_core::cache_stats();
        heap_census_line();
        track_report();
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

#[secgate::entry(lib = "naming")]
pub fn get_buffer(desc: Descriptor) -> Result<ObjID> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client = lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;
    client.buffer_id()
}

#[secgate::entry(lib = "naming")]
pub fn close_handle(desc: Descriptor) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let service = NAMINGSERVICE.get().unwrap();

    let mut binding = service.handles.lock().unwrap();

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

#[secgate::entry(lib = "naming")]
pub fn put(desc: Descriptor, name_len: usize, id: ObjID) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client = lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.put(path, id)
}

#[secgate::entry(lib = "naming")]
pub fn mkns(desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client = lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.mkns(path, persist)
}

#[secgate::entry(lib = "naming")]
pub fn link(desc: Descriptor, name_len: usize, link_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;
    let link = inner.read_buffer_at(link_len, name_len)?;

    inner.session.link(path, link)
}

#[secgate::entry(lib = "naming")]
pub fn get(desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.get(path, flags)
}

#[secgate::entry(lib = "naming")]
pub fn get_inline(desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
    let t_entry = getphase::start();
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let t_lookup = getphase::lap(&t_entry);
    let inner = client.inner.lock().unwrap();
    let t_innerlock = getphase::lap(&t_entry);

    // `as_str`, not `to_path`: the path is already sitting in the gate's arguments and
    // `NameSession::get` wants a `&str` back out of whatever it is handed, so the `PathBuf` was an
    // allocate-and-free per lookup purely to change type. Kept because it is less work, not because
    // it is faster -- A/B'd at four rounds a side and the effect is below this instrument's
    // resolution (22 in this file).
    let res = inner.session.get(path.as_str()?, flags);
    getphase::record(t_lookup, t_innerlock, getphase::lap(&t_entry));
    res
}

/// Where a warm `get_inline` spends its time *inside the server*, so the gate call's own share can
/// be had by subtracting from `pagepar`'s NAME figure.
///
/// This exists because the NAME phase cannot resolve anything under about a microsecond (22), and
/// the open question -- 3 us solo against ~7 us at four threads on a path whose only work is a
/// sharded hash lookup -- lives below that. Three clock reads and four shared-cacheline RMWs per
/// call, which at four threads is itself a contention term: read the *shares*, and expect the
/// totals to be inflated relative to an uninstrumented run. sysperf.md round 6 is the cautionary
/// tale for believing otherwise.
mod getphase {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    pub const GETPHASE_STATS: bool = false;

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static LOOKUP: AtomicU64 = AtomicU64::new(0);
    static INNERLOCK: AtomicU64 = AtomicU64::new(0);
    static GET: AtomicU64 = AtomicU64::new(0);

    pub fn start() -> Option<Instant> {
        GETPHASE_STATS.then(Instant::now)
    }

    /// Nanoseconds since `t`, cumulative -- the caller differences them.
    pub fn lap(t: &Option<Instant>) -> u64 {
        t.map_or(0, |t| t.elapsed().as_nanos() as u64)
    }

    pub fn record(lookup: u64, innerlock: u64, total: u64) {
        if !GETPHASE_STATS {
            return;
        }
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let l = LOOKUP.fetch_add(lookup, Ordering::Relaxed) + lookup;
        let i = INNERLOCK.fetch_add(innerlock.saturating_sub(lookup), Ordering::Relaxed)
            + innerlock.saturating_sub(lookup);
        let g = GET.fetch_add(total.saturating_sub(innerlock), Ordering::Relaxed)
            + total.saturating_sub(innerlock);
        if n.is_power_of_two() {
            twizzler_abi::klog_println!(
                "GETPHASE {} calls: caller+handles {} ns, inner-lock {} ns, session-get {} ns \
                 (per call, means)",
                n,
                l / n,
                i / n,
                g / n,
            );
        }
    }
}

#[secgate::entry(lib = "naming")]
pub fn rename(desc: Descriptor, old_len: usize, new_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let old_path = inner.read_buffer(old_len)?;
    let new_path = inner.read_buffer_at(new_len, old_len)?;

    inner.session.rename(old_path, new_path)
}

#[secgate::entry(lib = "naming")]
pub fn remove(desc: Descriptor, name_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.remove(path)?;

    Ok(())
}

#[secgate::entry(lib = "naming")]
pub fn enumerate_names(
    desc: Descriptor,
    name_len: usize,
    skip: usize,
    count: usize,
) -> Result<usize> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    // TODO: make not bad
    let vec1 = inner.session.enumerate_namespace(path, skip, count)?;
    let len = vec1.len();

    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    let n = inner.buffer_mut()?.write(slice);
    inner.note_used(n);

    Ok(len)
}

#[secgate::entry(lib = "naming")]
pub fn enumerate_names_nsid(
    desc: Descriptor,
    id: ObjID,
    skip: usize,
    count: usize,
) -> Result<usize> {
    let t_lock = std::time::Instant::now();
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();
    let lock_ns = t_lock.elapsed().as_nanos() as u64;

    // TODO: make not bad
    let t_items = std::time::Instant::now();
    let vec1 = inner.session.enumerate_namespace_nsid(id, skip, count)?;
    let items_ns = t_items.elapsed().as_nanos() as u64;
    let len = vec1.len();

    let t_write = std::time::Instant::now();
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    let n = inner.buffer_mut()?.write(slice);
    inner.note_used(n);
    srvenumstats::record(
        lock_ns,
        items_ns,
        t_write.elapsed().as_nanos() as u64,
        len as u64,
    );

    Ok(len)
}

// Temporary instrumentation for the directory-enumeration latency hunt (pagerperf.md).
mod srvenumstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static ENTRIES: AtomicU64 = AtomicU64::new(0);
    static LOCK: AtomicU64 = AtomicU64::new(0);
    static ITEMS: AtomicU64 = AtomicU64::new(0);
    static WRITE: AtomicU64 = AtomicU64::new(0);

    pub fn record(lock: u64, items: u64, write: u64, entries: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let l = LOCK.fetch_add(lock, Ordering::Relaxed) + lock;
        let i = ITEMS.fetch_add(items, Ordering::Relaxed) + items;
        let w = WRITE.fetch_add(write, Ordering::Relaxed) + write;
        let e = ENTRIES.fetch_add(entries, Ordering::Relaxed) + entries;
        if secgate::statcadence::report_now(n) {
            secgate::statline!(
                "SRVENUMSTATS {} calls, {} entries: lock {} us, items {} us, write {} us",
                n,
                e,
                l / 1000,
                i / 1000,
                w / 1000,
            );
        }
    }
}

#[secgate::entry(lib = "naming")]
pub fn change_namespace(desc: Descriptor, name_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let mut inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.change_namespace(path)
}


// ---- naming-srv's own heap census --------------------------------------------------------------
//
// `leakcheck` proved this service's heap grows ~134 KB per compartment load while the *spawner's*
// heap is flat, its handle table is flat, and its namespace cache is bit-identical across 224 opens.
// Three mechanisms proposed, three measured and dead. The remaining question is what is actually
// being retained, and the instrument that answers it already exists -- the per-size-class census in
// `twz-rt` -- it has simply only ever been armed inside leakcheck. The counters are per-compartment
// statics, so arming it here counts *this* compartment's allocations and nobody else's.
//
// Reported as a delta against the previous report, not a running total: a cumulative figure read
// every 32 opens attributes all of history to the latest window, which is the same mistake as
// reading a boot-long counter per op.

unsafe extern "C" {
    fn __twz_rt_diag_heap_census(out: *mut u64, n: usize) -> usize;
    fn __twz_rt_diag_heap_census_arm() -> u64;
    fn __twz_rt_diag_heap_track_arm(lo: usize, hi: usize);
    fn __twz_rt_diag_heap_track_dump(out: *mut u64, n: usize) -> usize;
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
    twizzler_abi::klog_println!(
        "NAMING-HEAPOBJ main={} early={}",
        buf[n - 2],
        buf[n - 1]
    );
    for i in (0..n - 2).step_by(3) {
        twizzler_abi::klog_println!(
            "NAMING-HEAPOBJ-ID slot={} id={:x}{:016x} kind={}",
            buf[i],
            buf[i + 1],
            buf[i + 2],
            if (i / 3) < buf[n - 2] as usize { "main" } else { "early" }
        );
    }
}

const CENSUS_BRANCH: usize = 16;
const CENSUS_CLASSES: usize = 32;
const CENSUS_WORDS: usize = CENSUS_BRANCH + CENSUS_CLASSES * 4;

static CENSUS_PREV: Mutex<Option<[u64; CENSUS_WORDS]>> = Mutex::new(None);

/// Arm the runtime's census for this compartment. Called once, from `namer_start`.
fn heap_census_arm() {
    let was = unsafe { __twz_rt_diag_heap_census_arm() };
    twizzler_abi::klog_println!("NAMING-HEAPCENSUS armed was_already_armed={}", was);
    track_arm();
}

// ---- live-block sizes in the `le=512` class ---------------------------------------------------
//
// naming retains ~173 blocks of ~376 B across a run -- roughly one per four compartment loads,
// and the largest repeating population it has once the one-time 512 KiB `early_nots` block is set
// aside. The census names a size *class*; this names the exact sizes inside it, which is a
// greppable fingerprint for a call site. Same instrument that identified the monitor's 3328-byte
// `UpcallFrame`.
//
// Sizes only, no dereferencing: a fault here takes naming down with it.

const TRACK_LO: usize = 257;
const TRACK_HI: usize = 512;
const TRACK_WORDS: usize = 2048;

static TRACK_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static mut TRACK_BUF: [u64; TRACK_WORDS] = [0; TRACK_WORDS];

/// One allocation *inside* the tracked window, made and freed immediately after arming.
///
/// An empty table reads identically whether nothing was retained or the hook never fires. The
/// monitor's first attempt at this instrument had a control of 131072 B against a [2049, 4096]
/// window -- it exercised the census and not the tracker, and `live=0` was uninterpretable until
/// the call sites were read by hand. The control has to sit in the range being reported on.
const TRACK_CONTROL_SIZE: usize = 384;

fn track_arm() {
    unsafe { __twz_rt_diag_heap_track_arm(TRACK_LO, TRACK_HI) };
    // Push, then escape the pointer: `with_capacity` + `black_box(capacity)` is elided outright.
    let mut v: Vec<u8> = Vec::with_capacity(TRACK_CONTROL_SIZE);
    v.push(0xa5);
    core::hint::black_box(v.as_ptr());
    drop(v);
    twizzler_abi::klog_println!(
        "NAMING-TRACK-ARM lo={} hi={} control_alloc_and_free={}",
        TRACK_LO,
        TRACK_HI,
        TRACK_CONTROL_SIZE
    );
}

/// Histogram of live block sizes in `[TRACK_LO, TRACK_HI]`.
#[allow(static_mut_refs)]
fn track_report() {
    use std::sync::atomic::Ordering::Relaxed;
    if TRACK_BUSY.swap(true, Relaxed) {
        return;
    }
    let n = unsafe { __twz_rt_diag_heap_track_dump(TRACK_BUF.as_mut_ptr(), TRACK_WORDS) };
    if n < 5 {
        twizzler_abi::klog_println!("NAMING-TRACK unavailable n={}", n);
        TRACK_BUSY.store(false, Relaxed);
        return;
    }
    let pairs = (n - 5) / 2;
    let mut size = [0u64; 32];
    let mut count = [0u64; 32];
    let mut distinct = 0usize;
    let mut overflow = 0u64;
    for i in 0..pairs {
        let sz = unsafe { TRACK_BUF[i * 2 + 1] };
        match (0..distinct).find(|&j| size[j] == sz) {
            Some(j) => count[j] += 1,
            None if distinct < 32 => {
                size[distinct] = sz;
                count[distinct] = 1;
                distinct += 1;
            }
            None => overflow += 1,
        }
    }
    let (inserted, removed, ovf, trunc) = unsafe {
        (
            TRACK_BUF[pairs * 2],
            TRACK_BUF[pairs * 2 + 1],
            TRACK_BUF[pairs * 2 + 2],
            TRACK_BUF[pairs * 2 + 4],
        )
    };
    twizzler_abi::klog_println!(
        "NAMING-TRACK live={} distinct={} unbinned={} inserted={} removed={} slot_overflow={} truncated={}",
        pairs, distinct, overflow, inserted, removed, ovf, trunc
    );
    // Biggest population first would need a sort; the set is <=32 and the reader can sort.
    for j in 0..distinct {
        twizzler_abi::klog_println!("NAMING-TRACK-SIZE bytes={} live={}", size[j], count[j]);
    }
    TRACK_BUSY.store(false, Relaxed);
}

/// One line: the branch counters, then the three classes with the largest net bytes this window.
fn heap_census_line() {
    let mut cur = [0u64; CENSUS_WORDS];
    let n = unsafe { __twz_rt_diag_heap_census(cur.as_mut_ptr(), CENSUS_WORDS) };
    if n != CENSUS_WORDS {
        twizzler_abi::klog_println!("NAMING-HEAPCENSUS unavailable (not armed)");
        return;
    }
    let mut guard = CENSUS_PREV.lock().unwrap();
    let prev = guard.unwrap_or([0u64; CENSUS_WORDS]);
    *guard = Some(cur);
    drop(guard);

    let d = |i: usize| cur[i] as i64 - prev[i] as i64;
    // Discarded frees are counted on their branch and never as frees, so a nonzero drop_* here
    // would mean this service's growth is the runtime throwing frees away rather than retention.
    twizzler_abi::klog_println!(
        "NAMING-HEAPCENSUS ferroc={}/{} early_cold={}/{} early_nots={}/{} drop_earlyptr={}/{} drop_nulltls={}/{} drop_nots={}/{}",
        d(0), d(8), d(1), d(9), d(2), d(10), d(5), d(13), d(6), d(14), d(7), d(15),
    );

    let mut rows: Vec<(usize, i64, i64, i64)> = Vec::new();
    for c in 0..CENSUS_CLASSES {
        let b = CENSUS_BRANCH + c * 4;
        let (ac, ab, fc, fb) = (d(b), d(b + 1), d(b + 2), d(b + 3));
        if ac == 0 && fc == 0 {
            continue;
        }
        rows.push((c, ac - fc, ab - fb, ac));
    }
    rows.sort_by_key(|r| -(r.2.abs()));
    for (c, net_count, net_bytes, allocs) in rows.into_iter() {
        twizzler_abi::klog_println!(
            "NAMING-HEAPCENSUS-CLASS le={} allocs={} net_count={} net_bytes={}",
            1u64 << c,
            allocs,
            net_count,
            net_bytes,
        );
    }

    // Cumulative since arm, alongside the per-window delta.
    //
    // A per-window row cannot be read as retention: a block allocated in window N and freed in
    // window N+1 shows as +1 then -1, so any single window over-reports. Only the running total
    // nets those out, and it is the figure that attributes the run. Both are printed because they
    // answer different questions -- the delta says "is it still happening", the total says "how
    // much". Reading the first as the second is the mistake this line exists to prevent.
    let mut tot: Vec<(usize, i64, i64)> = Vec::new();
    let mut tot_bytes: i64 = 0;
    for c in 0..CENSUS_CLASSES {
        let b = CENSUS_BRANCH + c * 4;
        let (ac, ab, fc, fb) = (
            cur[b] as i64,
            cur[b + 1] as i64,
            cur[b + 2] as i64,
            cur[b + 3] as i64,
        );
        if ac == 0 && fc == 0 {
            continue;
        }
        tot_bytes += ab - fb;
        tot.push((c, ac - fc, ab - fb));
    }
    tot.sort_by_key(|r| -(r.2.abs()));
    twizzler_abi::klog_println!(
        "NAMING-HEAPCENSUS-TOTAL net_bytes={} classes={}",
        tot_bytes,
        tot.len()
    );
    for (c, net_count, net_bytes) in tot.into_iter() {
        twizzler_abi::klog_println!(
            "NAMING-HEAPCENSUS-TOTALCLASS le={} net_count={} net_bytes={}",
            1u64 << c,
            net_count,
            net_bytes,
        );
    }
}
