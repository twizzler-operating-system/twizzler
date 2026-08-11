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

    let next = sbo.objs.pop().unwrap();
    // TODO: discard all object pages.
    Ok(next)
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
}

impl ClientInner<'_> {
    fn buffer(&self) -> Result<&SimpleBuffer> {
        self.buffer.as_ref().ok_or(ArgumentError::BadHandle.into())
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
        Some(self.inner.into_inner().unwrap().buffer?.into_handle())
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
    fn read_buffer(&self, name_len: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let mut buf = vec![0; name_len];
        self.buffer()?.read(&mut buf);
        Ok(PathBuf::from(
            String::from_utf8(buf).map_err(|_| ErrorKind::InvalidFilename)?,
        ))
    }

    fn read_buffer_at(&self, name_len: usize, off: usize) -> Result<PathBuf> {
        if name_len >= PATH_MAX {
            return Err(ArgumentError::InvalidArgument.into());
        }
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
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    Ok(NAMINGSERVICE
        .get_or_create(|_| {
            let namer = Namer::new_with(bootstrap)
                .or::<ErrorKind>(Ok(Namer::new()))
                .unwrap();
            namer.names.root_session().mkns("/initrd", false).unwrap();
            for n in get_kernel_init_info().names() {
                if n.name() != ".." && n.name() != "." {
                    namer
                        .names
                        .root_session()
                        .put(&format!("/initrd/{}", n.name()), n.id())
                        .unwrap();
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

    binding
        .insert(info.source_context().unwrap_or(0.into()), Arc::new(client))
        .ok_or(ResourceError::OutOfResources.into())
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
    let inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.put(path, id)
}

#[secgate::entry(lib = "naming")]
pub fn mkns(desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client = lookup_client(info.source_context().unwrap_or(0.into()), desc)
        .ok_or(ArgumentError::BadHandle)?;
    let inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    inner.session.mkns(path, persist)
}

#[secgate::entry(lib = "naming")]
pub fn link(desc: Descriptor, name_len: usize, link_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;
    let link = inner.read_buffer_at(link_len, name_len)?;

    inner.session.link(path, link)
}

#[secgate::entry(lib = "naming")]
pub fn get(desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {
    let t_start = std::time::Instant::now();
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();
    let lock_ns = t_start.elapsed().as_nanos() as u64;

    let path = inner.read_buffer(name_len)?;

    let t_namei = std::time::Instant::now();
    let res = inner.session.get(path, flags);
    getstats::record(lock_ns, t_namei.elapsed().as_nanos() as u64);
    res
}

#[secgate::entry(lib = "naming")]
pub fn get_inline(desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
    let t_start = std::time::Instant::now();
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();
    let lock_ns = t_start.elapsed().as_nanos() as u64;

    let path = path.to_path()?;

    let t_namei = std::time::Instant::now();
    let res = inner.session.get(path, flags);
    getstats::record(lock_ns, t_namei.elapsed().as_nanos() as u64);
    res
}

// Temporary instrumentation for the File::open latency hunt (pagerperf.md).
mod getstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static LOCK: AtomicU64 = AtomicU64::new(0);
    static NAMEI: AtomicU64 = AtomicU64::new(0);

    pub fn record(lock: u64, namei: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let l = LOCK.fetch_add(lock, Ordering::Relaxed) + lock;
        let m = NAMEI.fetch_add(namei, Ordering::Relaxed) + namei;
        if n.is_power_of_two() {
            twizzler_abi::klog_println!(
                "GETSTATS {} gets: handle-lock {} us, namei {} us (per get: {} us)",
                n,
                l / 1000,
                m / 1000,
                (l + m) / (n * 1000),
            );
        }
    }
}

#[secgate::entry(lib = "naming")]
pub fn rename(desc: Descriptor, old_len: usize, new_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();

    let old_path = inner.read_buffer(old_len)?;
    let new_path = inner.read_buffer_at(new_len, old_len)?;

    inner.session.rename(old_path, new_path)
}

#[secgate::entry(lib = "naming")]
pub fn remove(desc: Descriptor, name_len: usize) -> Result<()> {
    let info = secgate::get_caller().ok_or(SecurityError::InvalidGate)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();

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
    let inner = client.inner.lock().unwrap();

    let path = inner.read_buffer(name_len)?;

    // TODO: make not bad
    let vec1 = inner.session.enumerate_namespace(path, skip, count)?;
    let len = vec1.len();

    let mut buffer = SimpleBuffer::new(inner.buffer()?.handle().clone());
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    buffer.write(slice);

    Ok(len)
}

#[secgate::entry(lib = "naming")]
pub fn enumerate_names_nsid(
    desc: Descriptor,
    id: ObjID,
    skip: usize,
    count: usize,
) -> Result<usize> {
    let info = secgate::get_caller().ok_or(TwzError::INVALID_ARGUMENT)?;
    let client =
        lookup_client(info.source_context().unwrap_or(0.into()), desc).ok_or(ErrorKind::Other)?;
    let inner = client.inner.lock().unwrap();

    // TODO: make not bad
    let vec1 = inner.session.enumerate_namespace_nsid(id, skip, count)?;
    let len = vec1.len();

    let mut buffer = SimpleBuffer::new(inner.buffer()?.handle().clone());
    let slice = unsafe {
        std::slice::from_raw_parts(
            vec1.as_ptr() as *const u8,
            len * std::mem::size_of::<NsNode>(),
        )
    };
    buffer.write(slice);

    Ok(len)
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
