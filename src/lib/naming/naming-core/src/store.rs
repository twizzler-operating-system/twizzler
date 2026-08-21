use core::str;
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use bitflags::bitflags;
use ext::ExtNamespace;
use nsobj::NamespaceObject;
use pager_dynamic::objid_to_ino;
use twizzler::marker::Invariant;
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, NamingError},
    object::ObjID,
};

use crate::{Result, MAX_KEY_SIZE};

mod ext;
/// DIAG: external-namespace cache sizes. See [`ext::cache_stats`].
pub use ext::cache_stats;
mod nsobj;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum NsNodeKind {
    Namespace,
    Object,
    SymLink,
}
unsafe impl Invariant for NsNodeKind {}

const NSID_EXTERNAL: ObjID = ObjID::new(1);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, twizzler::Invariant)]
#[repr(C)]
pub struct NsNode {
    name: [u8; MAX_KEY_SIZE],
    pub id: ObjID,
    pub kind: NsNodeKind,
    name_len: u32,
    link_len: u32,
}

impl NsNode {
    pub fn new<P: AsRef<Path>, L: AsRef<Path>>(
        kind: NsNodeKind,
        id: ObjID,
        name: P,
        link_name: Option<L>,
    ) -> Result<Self> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
        if name.len() > MAX_KEY_SIZE {
            return Err(ArgumentError::InvalidArgument.into());
        }
        Ok(if let Some(link_name) = link_name {
            let lname = link_name.as_ref().as_os_str().as_encoded_bytes();
            if lname.len() + name.len() > MAX_KEY_SIZE {
                return Err(ArgumentError::InvalidArgument.into());
            }
            let mut cname = [0; MAX_KEY_SIZE];
            cname[0..name.len()].copy_from_slice(&name);
            cname[name.len()..(name.len() + lname.len())].clone_from_slice(&lname);
            Self {
                kind: NsNodeKind::SymLink,
                name: cname,
                id,
                name_len: name.len() as u32,
                link_len: lname.len() as u32,
            }
        } else {
            let mut cname = [0; MAX_KEY_SIZE];
            cname[0..name.len()].copy_from_slice(&name);
            Self {
                kind,
                id,
                name: cname,
                name_len: name.len() as u32,
                link_len: 0,
            }
        })
    }

    pub fn ns<P: AsRef<Path>>(name: P, id: ObjID) -> Result<Self> {
        Self::new::<_, P>(NsNodeKind::Namespace, id, name, None)
    }

    pub fn obj<P: AsRef<Path>>(name: P, id: ObjID) -> Result<Self> {
        Self::new::<_, P>(NsNodeKind::Object, id, name, None)
    }

    pub fn symlink<P: AsRef<Path>, L: AsRef<Path>>(name: P, lname: L) -> Result<Self> {
        Self::new(NsNodeKind::SymLink, 0.into(), name, Some(lname))
    }

    pub fn name(&self) -> Result<&str> {
        let bytes = &self.name[0..(self.name_len as usize)];
        str::from_utf8(bytes).map_err(|_| GenericError::Internal.into())
    }

    pub fn readlink(&self) -> Result<&str> {
        if self.kind != NsNodeKind::SymLink {
            return Err(NamingError::WrongNameKind.into());
        }
        let bytes =
            &self.name[(self.name_len as usize)..(self.name_len as usize + self.link_len as usize)];
        str::from_utf8(bytes).map_err(|_| GenericError::Internal.into())
    }
}

// Temporary instrumentation for the directory-enumeration latency hunt (pagerperf.md): opening a
// namespace by id maps its object, which is a monitor gate call on a cache miss.
//
// Gated by a const rather than by `statcadence::STATS_ON`, which suppresses the *output* and leaves
// the work: this cost two `Instant::now()` and three shared-cacheline RMWs on every readdir chunk,
// which is the shape sysperf.md round 6 caught doing more harm than the thing it measured.
mod nsidstats {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };

    pub const NSID_STATS: bool = false;

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static OPEN: AtomicU64 = AtomicU64::new(0);
    static ITEMS: AtomicU64 = AtomicU64::new(0);

    /// `None` unless the instrument is on, so the clock read folds away with it.
    pub fn start() -> Option<Instant> {
        NSID_STATS.then(Instant::now)
    }

    pub fn elapsed(t: Option<Instant>) -> u64 {
        t.map_or(0, |t| t.elapsed().as_nanos() as u64)
    }

    pub fn record(open: u64, items: u64) {
        if !NSID_STATS {
            return;
        }
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let o = OPEN.fetch_add(open, Ordering::Relaxed) + open;
        let i = ITEMS.fetch_add(items, Ordering::Relaxed) + items;
        if n.is_power_of_two() {
            twizzler_abi::klog_println!(
                "NSIDSTATS {} enumerates: open-ns {} us, items {} us",
                n,
                o / 1000,
                i / 1000,
            );
        }
    }
}

/// A memo of resolved paths, so a repeated lookup does not re-walk -- and re-lock -- the tree.
///
/// `namei` takes on the order of ten process-global mutexes for a warm four-component path: the
/// root object's pair, twice over because an absolute symlink like `/sysroot` restarts the walk at
/// the root; the external-namespace registry, once per component in `open_namespace`; and each
/// namespace's own cache. That is why the namer's cost grows with thread count -- 2.3 us solo
/// against 8.4 us at four threads (pagerperf.md 21) -- and why 21 concluded the fix had to be
/// taking fewer locks rather than cheaper ones. A hit here takes exactly one, and which one is
/// chosen by the path's hash, so threads looking up different paths do not meet at all.
///
/// Correctness rests on a single counter: every mutation of every namespace bumps `GEN`, and an
/// entry is good only for the generation it was recorded in. That makes this strictly more
/// conservative than the per-namespace caches underneath it, which invalidate only the namespace
/// that changed.
///
/// What the counter cannot see is a change made to the backing store from outside this compartment
/// -- object-store writes its own files into this tree. `NsCache::by_name` is already exposed to
/// exactly that and holds positive bindings forever; a whole *path* is more exposed than one
/// component, because an intermediate directory can be rebound under an entry that still looks
/// current. Since ext4 reuses inode numbers, the failure that would produce is not a slightly old
/// answer but the wrong object, so entries here also expire -- cheap insurance the layer below
/// does not buy, and one clock read on a path that costs microseconds.
mod memo {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
        num::NonZeroUsize,
        sync::{
            atomic::{AtomicU64, Ordering},
            Mutex,
        },
        time::{Duration, Instant},
    };

    use lru::LruCache;
    use twizzler_rt_abi::object::ObjID;

    use super::{GetFlags, NsNode};

    /// A/B knob. `false` restores the pre-memo path exactly.
    pub const MEMO_ON: bool = true;
    /// Hit/miss reporting. Off by default and const-folded away when it is: round 6 of sysperf.md
    /// is a catalogue of counters that became the thing they were measuring.
    const MEMO_STATS: bool = false;

    const SHARDS: usize = 16;
    const PER_SHARD: usize = 32;

    /// How long a resolved path stays believed, against changes made to the backing store from
    /// outside this compartment. Mirrors `ext::NEGATIVE_TTL`: far longer than the repeat-lookup
    /// patterns this exists for (a loader walking a search path, a readdir followed by opens),
    /// far shorter than a human noticing a rename.
    const TTL: Duration = Duration::from_secs(1);

    static GEN: AtomicU64 = AtomicU64::new(1);
    static TABLE: [Mutex<Option<LruCache<u64, Entry>>>; SHARDS] =
        [const { Mutex::new(None) }; SHARDS];

    struct Entry {
        gen: u64,
        recorded: Instant,
        ns: ObjID,
        flags: GetFlags,
        path: String,
        node: NsNode,
    }

    /// What a lookup *is*: where it starts, the path, and the flags that change what the path
    /// means. Hashed rather than kept as the key so that a lookup allocates nothing; a hit has to
    /// prove itself against the entry's own copy before it is believed.
    fn key(ns: ObjID, path: &str, flags: GetFlags) -> u64 {
        let mut h = DefaultHasher::new();
        ns.raw().hash(&mut h);
        path.hash(&mut h);
        flags.bits().hash(&mut h);
        h.finish()
    }

    /// Read before the walk, presented back with the result. A mutation that lands *during* the
    /// walk leaves the recorded generation behind the current one, so the entry is stale the
    /// moment it is written rather than being served once from a tree that has already moved.
    pub fn generation() -> u64 {
        GEN.load(Ordering::Acquire)
    }

    /// Retire every entry. Called from the mutation primitives themselves, not from their callers,
    /// so a new caller cannot forget to.
    pub fn invalidate() {
        GEN.fetch_add(1, Ordering::Release);
    }

    /// Why a lookup did not get an answer. The split is the point: a memo that misses because it
    /// has not seen a path yet is working as designed, one that misses because the generation moved
    /// is being destroyed by write traffic elsewhere in the tree, and one that misses on expiry is
    /// paying for a staleness bound it may not need. Only the first is fixed by running longer.
    #[derive(Clone, Copy)]
    enum Miss {
        /// No entry for this key -- first sight, or evicted by its shard's LRU.
        Unseen,
        /// An entry was there, and a mutation somewhere in the tree retired it.
        StaleGen,
        /// An entry was there and current, and older than `TTL`.
        Expired,
        /// The key collided: same hash, different lookup.
        Collision,
    }

    static HITS: AtomicU64 = AtomicU64::new(0);
    static UNSEEN: AtomicU64 = AtomicU64::new(0);
    static STALE_GEN: AtomicU64 = AtomicU64::new(0);
    static EXPIRED: AtomicU64 = AtomicU64::new(0);
    static COLLISION: AtomicU64 = AtomicU64::new(0);

    /// Folds away entirely with `MEMO_STATS` off, atomics included.
    fn note(miss: Option<Miss>) {
        if !MEMO_STATS {
            return;
        }
        match miss {
            None => HITS.fetch_add(1, Ordering::Relaxed),
            Some(Miss::Unseen) => UNSEEN.fetch_add(1, Ordering::Relaxed),
            Some(Miss::StaleGen) => STALE_GEN.fetch_add(1, Ordering::Relaxed),
            Some(Miss::Expired) => EXPIRED.fetch_add(1, Ordering::Relaxed),
            Some(Miss::Collision) => COLLISION.fetch_add(1, Ordering::Relaxed),
        };
        let h = HITS.load(Ordering::Relaxed);
        let u = UNSEEN.load(Ordering::Relaxed);
        let s = STALE_GEN.load(Ordering::Relaxed);
        let e = EXPIRED.load(Ordering::Relaxed);
        let c = COLLISION.load(Ordering::Relaxed);
        let total = h + u + s + e + c;
        if total.is_power_of_two() {
            // One write, because `klog_println!` interleaves under concurrency and a torn report
            // loses every counter in it.
            twizzler_abi::klog_println!(
                "MEMOSTATS {} lookups: {} hits ({}%), misses: {} unseen, {} stale-gen, {} expired, \
                 {} collision; gen {}",
                total,
                h,
                h * 100 / total.max(1),
                u,
                s,
                e,
                c,
                generation(),
            );
        }
    }

    pub fn lookup(ns: ObjID, path: &str, flags: GetFlags) -> Option<NsNode> {
        let k = key(ns, path, flags);
        let gen = generation();
        let mut miss = Some(Miss::Unseen);
        let node = (|| {
            let mut shard = TABLE[(k as usize) % SHARDS].lock().ok()?;
            let entry = shard.as_mut()?.get(&k)?;
            // Order matters only for the diagnosis: the cheapest disqualifier that applies is the
            // one reported, and identity is checked before staleness so a collision is never
            // reported as write traffic.
            if entry.ns != ns || entry.flags != flags || entry.path != path {
                miss = Some(Miss::Collision);
                return None;
            }
            if entry.gen != gen {
                miss = Some(Miss::StaleGen);
                return None;
            }
            if entry.recorded.elapsed() >= TTL {
                miss = Some(Miss::Expired);
                return None;
            }
            miss = None;
            Some(entry.node)
        })();
        note(miss);
        node
    }

    pub fn record(gen: u64, ns: ObjID, path: &str, flags: GetFlags, node: NsNode) {
        let k = key(ns, path, flags);
        let Ok(mut shard) = TABLE[(k as usize) % SHARDS].lock() else {
            return;
        };
        let cache =
            shard.get_or_insert_with(|| LruCache::new(NonZeroUsize::new(PER_SHARD).unwrap()));
        cache.put(
            k,
            Entry {
                gen,
                recorded: Instant::now(),
                ns,
                flags,
                path: path.to_string(),
                node,
            },
        );
    }
}

pub(crate) use memo::invalidate as invalidate_memo;

/// How this binary was built, printed once at startup so a transcript identifies which arm of an
/// A/B produced it.
///
/// A boot's kernel command line proves the *image* was the one asked for; it does not prove the
/// image contains the binaries you built, which is the failure mode when a shared build tree hands
/// one session's artifacts to another session's sweep. A marker whose text differs between arms
/// does prove it, and costs one line per boot.
pub fn memo_config() -> &'static str {
    if memo::MEMO_ON {
        "memo=on shards=16x32 ttl=1s"
    } else {
        "memo=off"
    }
}

/// The name `remove`/`rename` unlinks, taken from the request path rather than from the node the
/// walk landed on.
///
/// The two differ for a path whose last component is not a plain name: "foo/.." resolves to the
/// node for "foo" *as seen from its parent*, so unlinking by the resolved node's name deletes
/// "foo" itself, and "." resolves to a namespace's own self-entry, which is written once at
/// creation and never restored. Both are `EINVAL` under POSIX, and so are they here.
fn unlink_name(path: &Path) -> Result<&str> {
    // `components` folds a trailing "." away ("foo/." -> "foo"), so that one has to be caught on
    // the raw path; every other non-name ending survives as a non-Normal final component.
    if path.as_os_str().as_encoded_bytes().ends_with(b"/.") {
        return Err(ArgumentError::InvalidArgument.into());
    }
    match path.components().next_back() {
        Some(Component::Normal(name)) => name
            .to_str()
            .ok_or_else(|| ArgumentError::InvalidArgument.into()),
        _ => Err(ArgumentError::InvalidArgument.into()),
    }
}

#[derive(Clone)]
struct ParentInfo {
    ns: Arc<dyn Namespace>,
    name_in_parent: String,
}

impl ParentInfo {
    fn new(ns: Arc<dyn Namespace>, name_in_parent: impl ToString) -> Self {
        Self {
            ns,
            name_in_parent: name_in_parent.to_string(),
        }
    }
}

trait Namespace {
    fn open(id: ObjID, persist: bool, parent_info: Option<ParentInfo>) -> Result<Self>
    where
        Self: Sized;

    fn find(&self, name: &str) -> Option<NsNode>;

    /// Bind `node`, failing with `AlreadyExists` if the name is taken. Atomic with respect to
    /// other operations on the same namespace *object*, which is not the same thing as on the same
    /// `Namespace` value -- see `nsobj::OBJ_LOCKS`.
    fn insert(&self, node: NsNode) -> Result<()>;

    /// Bind `node`, evicting any entry of the same name. Atomic in the same sense as `insert`.
    fn replace(&self, node: NsNode) -> Result<()>;

    fn remove(&self, name: &str) -> Option<NsNode>;

    fn parent(&self) -> Option<&ParentInfo>;

    fn id(&self) -> ObjID;

    fn items(&self, skip: usize, count: usize) -> Vec<NsNode>;

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.items(0, usize::MAX).len()
    }

    fn persist(&self) -> bool;

    fn create_file(&self, name: &str) -> Result<NsNode>;
}

pub struct NameStore {
    nameroot: Arc<dyn Namespace>,
    dataroot: ObjID,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let this = NameStore {
            nameroot: Arc::new(NamespaceObject::new(false, None, None).unwrap()),
            dataroot: 0.into(),
        };
        this.nameroot
            .insert(NsNode::ns("ext", NSID_EXTERNAL).unwrap())
            .unwrap();
        this
    }

    // Loads in an existing object store from an Object ID
    pub fn new_with(id: ObjID) -> Result<NameStore> {
        let mut this = Self::new();
        this.nameroot.insert(NsNode::ns("data", id)?)?;
        this.dataroot = id;
        tracing::debug!(
            "new_with: data={}, data={:?}, root={}",
            id,
            this.nameroot.find("data"),
            this.id()
        );
        Ok(this)
    }

    // Loads in an existing object store from an Object ID
    pub fn new_with_root(id: ObjID) -> Result<NameStore> {
        let namespace = NamespaceObject::open(id, false, None)?;
        Ok(Self {
            nameroot: Arc::new(namespace),
            dataroot: id,
        })
    }

    pub fn id(&self) -> ObjID {
        self.nameroot.id()
    }

    // session is created from root
    pub fn new_session(&self, namespace: &Path) -> NameSession<'_> {
        let mut path = PathBuf::from("/");
        path.extend(namespace);
        let mut this = NameSession {
            store: self,
            working_ns: None,
        };
        this.change_namespace(namespace).unwrap();
        this
    }

    pub fn root_session(&self) -> NameSession<'_> {
        NameSession {
            store: self,
            working_ns: None,
        }
    }
}

pub struct NameSession<'a> {
    store: &'a NameStore,
    working_ns: Option<Arc<dyn Namespace>>,
}

impl NameSession<'_> {
    pub const MAX_SYMLINK_DEREF: usize = 32;
    fn open_namespace(
        &self,
        id: ObjID,
        persist: bool,
        parent_info: Option<ParentInfo>,
    ) -> Result<Arc<dyn Namespace>> {
        let is_dataroot = id == self.store.dataroot;
        Ok(if id == NSID_EXTERNAL || objid_to_ino(id.raw()).is_some() {
            Arc::new(ExtNamespace::open(id, persist, parent_info)?)
        } else {
            Arc::new(NamespaceObject::open(
                id,
                persist || is_dataroot,
                parent_info,
            )?)
        })
    }

    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<P: AsRef<Path>>(
        &self,
        namespace: Option<Arc<dyn Namespace>>,
        name: P,
        mut nr_derefs: usize,
        deref: bool,
    ) -> Result<(std::result::Result<NsNode, PathBuf>, Arc<dyn Namespace>)> {
        tracing::trace!("namei: {:?}", name.as_ref());

        let mut namespace = namespace.unwrap_or_else(|| {
            self.working_ns
                .as_ref()
                .unwrap_or(&self.store.nameroot)
                .clone()
        });

        // Peeked rather than collected: `is_last` and the trailing component are the only things
        // the walk ever wanted from the component list, and both are available without putting a
        // Vec on the heap for every lookup that misses the memo.
        let mut components = name.as_ref().components().peekable();
        if components.peek().is_none() {
            return Ok((Err("".into()), namespace));
        }
        tracing::trace!("start search {}", name.as_ref().display());

        let mut node = None;
        let mut last_component = None;
        while let Some(item) = components.next() {
            let is_last = components.peek().is_none();
            last_component = Some(item);
            match item {
                Component::Prefix(_) => continue,
                Component::RootDir => {
                    namespace = self.store.nameroot.clone();
                    node = Some(NsNode::ns("/", namespace.id())?);
                }
                Component::CurDir => {
                    node = namespace.find(".");
                }
                Component::ParentDir => {
                    if let Some(parent) = namespace.parent() {
                        node = Some(NsNode::ns(&parent.name_in_parent, parent.ns.id())?);
                        namespace = parent.ns.clone();
                    } else {
                        node = Some(namespace.find("..").ok_or(NamingError::NotFound)?);
                        let parent_info = ParentInfo::new(namespace, "..");
                        namespace = self.open_namespace(
                            node.as_ref().unwrap().id,
                            parent_info.ns.persist(),
                            Some(parent_info),
                        )?;
                    }
                }
                Component::Normal(os_str) => {
                    tracing::trace!(
                        "lookup component {} in {}",
                        os_str.to_str().ok_or(ArgumentError::InvalidArgument)?,
                        namespace.id(),
                    );
                    node = namespace.find(os_str.to_str().ok_or(ArgumentError::InvalidArgument)?);
                    let name = node.as_ref().map(|x| x.name());
                    tracing::trace!("found node: {:?} (is_last = {})", name, is_last);

                    // Did we find something?
                    let Some(mut thisnode) = node else {
                        tracing::trace!(
                            "failed to find component {:?}: (is_last = {})",
                            os_str.to_str(),
                            is_last
                        );
                        // Last component: return with this name, None.
                        if is_last {
                            return Ok((Err(os_str.into()), namespace));
                        } else {
                            return Err(NamingError::NotFound.into());
                        }
                    };
                    // If symlink, deref. But keep track of recursion.
                    if thisnode.kind == NsNodeKind::SymLink {
                        tracing::trace!("found symlink: {} {} {}", nr_derefs, deref, is_last);
                        if nr_derefs == 0 {
                            return Err(NamingError::LinkLoop.into());
                        }
                        if deref || !is_last {
                            let mut lcont = None;
                            while thisnode.kind == NsNodeKind::SymLink {
                                let ldname = thisnode.readlink()?;
                                tracing::trace!("search with: {}", ldname);
                                nr_derefs -= 1;
                                let (lnode, lc) = self.namei_exist(
                                    Some(namespace.clone()),
                                    ldname,
                                    nr_derefs,
                                    deref,
                                )?;
                                tracing::trace!("found lnode as {:?}", lnode);
                                node = Some(lnode);
                                thisnode = lnode;
                                lcont = Some(lc);
                            }
                            if !is_last {
                                if thisnode.kind != NsNodeKind::Namespace {
                                    return Err(NamingError::WrongNameKind.into());
                                }
                                // Parent is where the *target* was found, not where the link was:
                                // a later ".." has to walk the resolved path back up.
                                namespace = self.open_namespace(
                                    thisnode.id,
                                    lcont.as_ref().unwrap().persist(),
                                    Some(ParentInfo {
                                        ns: lcont.unwrap(),
                                        name_in_parent: thisnode.name()?.to_string(),
                                    }),
                                )?;
                            }
                        }
                    } else if !is_last {
                        if thisnode.kind != NsNodeKind::Namespace {
                            return Err(NamingError::WrongNameKind.into());
                        }
                        let parent_info = ParentInfo::new(namespace, thisnode.name()?);
                        namespace = self.open_namespace(
                            thisnode.id,
                            parent_info.ns.persist(),
                            Some(parent_info),
                        )?;
                    }
                }
            }
        }
        tracing::trace!("namei result: {:?}", node);

        if let Some(node) = node {
            Ok((Ok(node), namespace))
        } else {
            // Unwrap-Ok: we checked if it's empty earlier, so the loop ran at least once.
            Ok((Err(last_component.unwrap().as_os_str().into()), namespace))
        }
    }

    fn namei_exist<'a, P: AsRef<Path>>(
        &self,
        namespace: Option<Arc<dyn Namespace>>,
        name: P,
        nr_derefs: usize,
        deref: bool,
    ) -> Result<(NsNode, Arc<dyn Namespace>)> {
        let (n, ns) = self.namei(namespace, name, nr_derefs, deref)?;
        Ok((n.ok().ok_or(NamingError::NotFound)?, ns))
    }

    pub fn mkns<P: AsRef<Path>>(&self, name: P, persist: bool) -> Result<()> {
        let (node, container) = self.namei(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        let Err(name) = node else {
            return Err(NamingError::AlreadyExists.into());
        };
        let ns = NamespaceObject::new(
            persist,
            Some(container.id()),
            Some(ParentInfo::new(
                container.clone(),
                name.display().to_string(),
            )),
        )?;
        container.insert(NsNode::ns(name, ns.id())?)
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, id: ObjID) -> Result<()> {
        tracing::debug!("put {:?}: {}", name.as_ref(), id);
        let (node, container) = self.namei(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        let Err(name) = node else {
            return Err(NamingError::AlreadyExists.into());
        };

        container.insert(NsNode::obj(name, id)?)
    }

    /// Where a relative path starts. Part of the memo key: the same path means different things
    /// from different working namespaces.
    fn start_id(&self) -> ObjID {
        self.working_ns
            .as_ref()
            .unwrap_or(&self.store.nameroot)
            .id()
    }

    pub fn get<P: AsRef<Path>>(&self, name: P, flags: GetFlags) -> Result<NsNode> {
        tracing::debug!("get {:?}: {:?}", name.as_ref(), flags);
        // Only a plain lookup is memoized. CREATE mutates on a miss, and a path that is not UTF-8
        // has no key -- both fall through to the walk.
        let memoized = (memo::MEMO_ON && !flags.contains(GetFlags::CREATE))
            .then(|| name.as_ref().to_str())
            .flatten();
        let start = if memoized.is_some() {
            self.start_id()
        } else {
            ObjID::new(0)
        };
        if let Some(path) = memoized {
            if let Some(node) = memo::lookup(start, path, flags) {
                return Ok(node);
            }
        }
        let gen = memo::generation();

        let (node, container) = self.namei(
            None,
            &name,
            Self::MAX_SYMLINK_DEREF,
            flags.contains(GetFlags::FOLLOW_SYMLINK),
        )?;

        if flags.contains(GetFlags::CREATE) {
            if let Err(ref node_name) = node {
                return container
                    .create_file(node_name.to_str().ok_or(ArgumentError::InvalidArgument)?);
            }
        }
        let node = node.ok().ok_or(NamingError::NotFound)?;
        // Only a hit is remembered: a name that is absent now can appear, and nothing outside this
        // compartment tells us when it does.
        if let Some(path) = memoized {
            memo::record(gen, start, path, flags, node);
        }
        Ok(node)
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(
        &self,
        name: P,
        skip: usize,
        count: usize,
    ) -> Result<std::vec::Vec<NsNode>> {
        tracing::trace!("enumerate: {:?}", name.as_ref());
        let (node, container) = self.namei_exist(None, name, Self::MAX_SYMLINK_DEREF, true)?;
        if node.kind != NsNodeKind::Namespace {
            return Err(NamingError::WrongNameKind.into());
        }
        tracing::trace!("opening namespace: {}", node.id);
        let ns = self.open_namespace(
            node.id,
            false,
            Some(ParentInfo::new(container, node.name()?)),
        )?;
        let items = ns.items(skip, count);
        tracing::trace!("collected: {:?}", items);
        Ok(items)
    }

    pub fn enumerate_namespace_nsid(
        &self,
        id: ObjID,
        skip: usize,
        count: usize,
    ) -> Result<std::vec::Vec<NsNode>> {
        tracing::trace!("opening namespace-ensid: {} {} {}", id, skip, count);
        let t_open = nsidstats::start();
        let ns = self.open_namespace(id, false, None)?;
        let open_ns = nsidstats::elapsed(t_open);
        let t_items = nsidstats::start();
        let items = ns.items(skip, count);
        nsidstats::record(open_ns, nsidstats::elapsed(t_items));
        tracing::trace!("collected: {:?}", items);
        Ok(items)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        tracing::trace!("change_ns: {:?}", name.as_ref());
        let (node, container) = self.namei_exist(None, name, Self::MAX_SYMLINK_DEREF, true)?;
        match node.kind {
            NsNodeKind::Namespace => {
                self.working_ns = Some(if node.id == container.id() {
                    // A self-reference (".", or a path ending in it): namei resolves this
                    // without ever moving `namespace` off `container`, so `container` already
                    // *is* the target with its real parent chain intact. Rebuilding it via
                    // `open_namespace`/`ParentInfo::new(container, ..)` below would instead stamp
                    // a bogus self-referential parent onto a fresh instance, breaking any
                    // subsequent ".." lookup.
                    container
                } else {
                    self.open_namespace(
                        node.id,
                        container.persist(),
                        Some(ParentInfo::new(container, node.name()?)),
                    )?
                });
                Ok(())
            }
            _ => Err(NamingError::WrongNameKind.into()),
        }
    }

    pub fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        let unlink = unlink_name(name.as_ref())?;
        let (_node, container) = self.namei_exist(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        container
            .remove(unlink)
            .map(|_| ())
            .ok_or(NamingError::NotFound.into())
    }

    pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(&self, old: P, new: Q) -> Result<()> {
        tracing::debug!("rename: {:?} to {:?}", old.as_ref(), new.as_ref());
        let old_name = unlink_name(old.as_ref())?;
        let new_name = unlink_name(new.as_ref())?;

        // Look up the old entry (don't follow symlinks — we're moving the entry itself)
        let (old_node, old_container) =
            self.namei_exist(None, &old, Self::MAX_SYMLINK_DEREF, false)?;

        let (new_node, new_container) = self.namei(None, &new, Self::MAX_SYMLINK_DEREF, false)?;

        // Source and destination denote the same entry: the replace-then-remove below would remove
        // the copy it just made and report success, so there is nothing to do but say so.
        if old_container.id() == new_container.id() && old_name == new_name {
            return Ok(());
        }

        // Never clobber a namespace, empty or not. Eviction would orphan everything under it --
        // there is no recursive delete here -- and nothing reclaims the namespace object itself.
        if new_node.is_ok_and(|n| n.kind == NsNodeKind::Namespace) {
            return Err(NamingError::AlreadyExists.into());
        }

        // Create new entry preserving the old node's type and data
        let new_entry = if old_node.kind == NsNodeKind::SymLink {
            NsNode::new(
                NsNodeKind::SymLink,
                old_node.id,
                &new_name,
                Some(old_node.readlink()?),
            )?
        } else {
            NsNode::new::<_, &str>(old_node.kind, old_node.id, &new_name, None)?
        };

        tracing::trace!(
            "insert new entry: {:?} in container {}",
            new_entry,
            new_container.id()
        );
        // Insert at new location, then remove from old location
        new_container.replace(new_entry)?;
        old_container
            .remove(old_name)
            .map(|_| ())
            .ok_or(NamingError::NotFound.into())
    }

    pub fn link<P: AsRef<Path>, L: AsRef<Path>>(&self, name: P, link: L) -> Result<()> {
        let (node, container) = self.namei(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        let Err(name) = node else {
            return Err(NamingError::AlreadyExists.into());
        };

        container.insert(NsNode::symlink(name, link)?)
    }

    pub fn readlink<P: AsRef<Path>>(&self, name: P) -> Result<PathBuf> {
        let (node, _) = self.namei_exist(None, name, Self::MAX_SYMLINK_DEREF, false)?;
        node.readlink().map(PathBuf::from)
    }
}

bitflags! {
    #[derive(Clone, Copy, Default, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct GetFlags: u32 {
        const FOLLOW_SYMLINK = 1;
        const CREATE = 2;
    }
}
