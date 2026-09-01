use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, RwLock,
    },
    time::{Duration, Instant},
};

use lru::LruCache;
use pager_dynamic::{objid_to_ino, ExternalKind, PagerHandle};
use secgate::TwzError;
use twizzler::object::ObjID;
use twizzler_rt_abi::error::NamingError;

use super::{Namespace, NsNode, ParentInfo};
use crate::{NsNodeKind, Result};

/// Entries to pull in when extending what we know of a namespace's order. A readdir walks a
/// directory in buffer-sized chunks, so fetching only the chunk asked for costs a gate call per
/// chunk; reading a window ahead amortizes that without dragging in a whole large directory.
/// Deliberately well under `MAX_NAMES`, so one enumeration cannot evict everything looked up.
const PREFETCH: usize = 128;

/// Caps, per namespace. An `NsNode` is a fixed 288 bytes whatever the name's length, so a fully
/// populated namespace costs roughly 170KB and the whole cache is bounded by that times
/// `MAX_NAMESPACES`. Overflowing any of them degrades to what an uncached namespace does -- ask
/// the pager -- so they can be tuned down freely.
const MAX_NAMES: usize = 256;
const MAX_ORDER: usize = 256;
const MAX_ABSENT: usize = 64;
const MAX_NAMESPACES: usize = 16;

/// How long a name stays known-absent. Nothing tells us when a name appears from outside this
/// compartment -- object-store writes its own files into this tree -- so absence is only ever
/// believed briefly. Repeated probing of a missing name (a PATH walk, a loader searching several
/// directories) happens far inside this window; a create seconds later does not.
const NEGATIVE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct ExtNamespace {
    id: ObjID,
    parent_info: Option<ParentInfo>,
    cache: Arc<Mutex<NsCache>>,
}

/// What this compartment knows about one external namespace.
///
/// The two halves answer different questions. `by_name` is a partial map filled by any lookup or
/// enumeration, and is what makes opening an already-seen name free. `order` is the pager's own
/// dirent order, known contiguously from index 0; an enumeration is served from it only where it
/// covers the requested window, so that window served from here and served by the pager are the
/// same sequence, and a positional readdir cursor survives the switch between them.
///
/// Feeding `by_name` into an enumeration instead -- emitting what is cached and then asking the
/// pager for the rest -- cannot work behind that cursor: the split point moves as lookups populate
/// the cache, so a `find` between two chunks shifts an entry from the tail into the head, and the
/// caller both misses that entry and receives another one twice.
struct NsCache {
    by_name: LruCache<String, NsNode>,
    /// Names a lookup found missing, and when. Much smaller than `by_name`: it exists for the
    /// probe-several-directories-for-one-name pattern, where the same handful of misses repeat
    /// within a single operation.
    absent: LruCache<String, Instant>,
    order: Vec<NsNode>,
    /// `order` holds the whole namespace, so a name it lacks does not exist. Never set once
    /// `order` has hit `MAX_ORDER`, because then it is a prefix and not the whole thing.
    complete: bool,
    /// Bumped on every invalidation, so a fetch that raced one can tell and drop its result.
    generation: u64,
}

struct GlobalCache {
    namespaces: RwLock<BTreeMap<ObjID, Arc<Mutex<NsCache>>>>,
}

impl GlobalCache {
    fn get_namespace_cache(&self, id: ObjID) -> Arc<Mutex<NsCache>> {
        // Once per path component on every lookup, and after the first walk it is always a hit
        // that mutates nothing -- so the read guard is the path that matters here.
        if let Some(cache) = self.namespaces.read().unwrap().get(&id) {
            return cache.clone();
        }
        let mut namespaces = self.namespaces.write().unwrap();
        if let Some(cache) = namespaces.get(&id) {
            return cache.clone();
        }
        // Over the cap: drop every namespace nobody is currently holding. Evicting one that is
        // still open would split it in two, and an invalidation through one instance would leave
        // the other's entries stale -- so live namespaces stay, making this a high-water mark
        // rather than a hard limit.
        //
        // The ceiling is therefore however many namespaces are held open at once, and
        // `NameSession` holds its working namespace, so it is the number of live client sessions.
        // **Nothing here enforces that**: if sessions are ever retained (a client that dies without
        // closing its handle, or a `HandleMgr` that fails to reclaim one), this map has no bound at
        // all. `MAX_NAMESPACES` limits nothing on its own.
        //
        // This comment previously asserted the excess "is bounded by the number of client
        // sessions", which stated the intent as though it were an invariant and read as evidence
        // that the bound held. It cost a leak hunt an hour: the chain "retained handle pins a
        // session pins a namespace pins an unevictable cache" is exactly what the sentence
        // suggested, and it is measurably not what happens (`ns_cached` is flat at 7 with
        // `ns_pinned` 0 across 224 handle opens -- leakcheck.md, namecache1). A comment that
        // asserts a *bound* can go false with nothing nearby changing; one that describes a
        // *mechanism* goes stale visibly. Prefer the second.
        if namespaces.len() >= MAX_NAMESPACES {
            namespaces.retain(|_, cache| Arc::strong_count(cache) > 1);
        }
        let cache = Arc::new(Mutex::new(NsCache::new()));
        namespaces.insert(id, cache.clone());
        cache
    }

    /// Forget a namespace entirely. Caches are keyed by ObjID and ext4 reuses inode numbers, so a
    /// later directory landing on a removed one's inode would otherwise inherit its entries.
    fn forget(&self, id: ObjID) {
        let entry = self.namespaces.write().unwrap().remove(&id);
        if let Some(cache) = entry {
            cache.lock().unwrap().clear();
        }
    }
}

static GLOBAL_CACHE: GlobalCache = GlobalCache {
    namespaces: RwLock::new(BTreeMap::new()),
};

/// DIAG: `(namespaces cached, cached names summed, order entries summed, pinned namespaces)`.
///
/// `MAX_NAMESPACES` is a high-water mark and not a limit -- eviction only drops caches with
/// `strong_count == 1`, so a namespace any live `NameSession` still holds cannot be evicted, and
/// the comment on `get_namespace_cache` bounds the excess by the number of client sessions. The
/// fourth field is what makes that checkable rather than argued: a `namespaces` length far above
/// `MAX_NAMESPACES` with most of them pinned is that bound being exceeded.
pub fn cache_stats() -> (usize, usize, usize, usize) {
    let ns = GLOBAL_CACHE.namespaces.read().unwrap();
    let mut names = 0;
    let mut order = 0;
    let mut pinned = 0;
    for cache in ns.values() {
        if std::sync::Arc::strong_count(cache) > 1 {
            pinned += 1;
        }
        if let Ok(c) = cache.try_lock() {
            names += c.by_name.len();
            order += c.order.len();
        }
    }
    (ns.len(), names, order, pinned)
}

/// How many pager handles this compartment will open.
///
/// A handle is one descriptor and one `SimpleBuffer`, and every `PagerHandle` method writes its
/// argument at offset 0 of that buffer and reads the reply back from offset 0 -- which is why the
/// methods take `&mut self` and why a handle can carry exactly one call at a time. Concurrency
/// therefore means more handles, not a bigger buffer: the pager's gate signatures
/// (`lookup_external(desc, dir, namelen)` and friends) carry no offset, so the slot-sharding the
/// naming client uses on its own buffer is not available here without changing that ABI.
///
/// Eight, matching `BUFFER_NSLOTS` on the naming side. Opened lazily, and only by a caller that
/// found the earlier ones busy, so a serial workload still opens exactly one.
const PAGER_HANDLES: usize = 8;

/// This compartment's pager handles. One `SimpleBuffer` each, so they are independent.
///
/// This was a single handle behind one mutex, held across the pager gate call -- which on a miss
/// is a disk round trip. Since naming-core runs inside naming-srv, that serialized every external
/// (store-backed) lookup, create and enumeration in the whole system behind one lock, and
/// `items()` held it across an `enumerate_external` *plus* one `readlink_external` per unresolved
/// symlink in the directory.
static PAGER_POOL: [Mutex<Option<PagerHandle>>; PAGER_HANDLES] =
    [const { Mutex::new(None) }; PAGER_HANDLES];

/// Which slot the next *blocking* caller waits on, so an all-busy pool spreads its waiters
/// instead of piling them onto one handle.
static PAGER_ROTOR: AtomicUsize = AtomicUsize::new(0);

/// Borrow one of the compartment's pager handles, opening it on first use. `None` inside means the
/// pager could not be reached; retried on the next call.
///
/// Lock order is a pager handle, then an `NsCache`. Nothing may take a pager handle while holding
/// a cache lock, and no caller may hold two handles at once -- both unchanged from when this was a
/// single lock, and both still what keeps the order acyclic.
fn pager_handle() -> MutexGuard<'static, Option<PagerHandle>> {
    // In order, from the front: a workload with no concurrency keeps landing on slot 0 and never
    // opens a second handle. A poisoned slot reads as busy and is skipped rather than panicking
    // every caller after the first.
    for slot in PAGER_POOL.iter() {
        if let Ok(guard) = slot.try_lock() {
            return open_handle(guard);
        }
    }
    // Every handle is in use. Block on one rather than spinning; the rotor keeps concurrent
    // waiters off a single slot.
    let idx = PAGER_ROTOR.fetch_add(1, Ordering::Relaxed) % PAGER_HANDLES;
    open_handle(PAGER_POOL[idx].lock().unwrap())
}

fn open_handle(
    mut guard: MutexGuard<'static, Option<PagerHandle>>,
) -> MutexGuard<'static, Option<PagerHandle>> {
    if guard.is_none() {
        *guard = PagerHandle::new();
    }
    guard
}

impl NsCache {
    fn new() -> Self {
        let cap = |n: usize| NonZeroUsize::new(n).expect("cache caps must be non-zero");
        Self {
            by_name: LruCache::new(cap(MAX_NAMES)),
            absent: LruCache::new(cap(MAX_ABSENT)),
            order: Vec::new(),
            complete: false,
            generation: 0,
        }
    }

    fn lookup(&mut self, name: &str) -> Option<NsNode> {
        self.by_name.get(name).copied()
    }

    /// Whether `name` was recently found missing. Expired entries are dropped on the way past, so
    /// a name probed once and never again does not hold its slot against a live one.
    fn known_absent(&mut self, name: &str) -> bool {
        match self.absent.get(name).copied() {
            Some(seen) if seen.elapsed() < NEGATIVE_TTL => true,
            Some(_) => {
                self.absent.pop(name);
                false
            }
            None => false,
        }
    }

    fn cache_absent(&mut self, name: &str) {
        self.absent.put(name.to_string(), Instant::now());
    }

    /// The requested window, if `order` is known to cover it.
    fn window(&self, skip: usize, count: usize) -> Option<Vec<NsNode>> {
        let end = skip.saturating_add(count);
        if !self.complete && end > self.order.len() {
            return None;
        }
        Some(
            self.order
                .get(skip..end.min(self.order.len()))
                .unwrap_or_default()
                .to_vec(),
        )
    }

    fn cache_node(&mut self, node: NsNode) {
        if let Ok(name) = node.name() {
            self.absent.pop(name);
            self.by_name.put(name.to_string(), node);
        }
    }

    /// Whether the order can still grow. Once it stops, enumeration past what is held goes to the
    /// pager, so there is no point reading ahead for it either.
    fn order_has_room(&self) -> bool {
        self.order.len() < MAX_ORDER
    }

    /// Fold a window the pager just returned back in. `positional` says the window is a
    /// one-for-one image of the pager's entries: if we had to drop one, our indices no longer line
    /// up with the pager's and the window can only contribute names.
    fn record(&mut self, at: usize, nodes: &[NsNode], hit_end: bool, positional: bool) {
        for node in nodes {
            self.cache_node(*node);
        }
        if !positional || at != self.order.len() {
            return;
        }
        let room = MAX_ORDER - self.order.len();
        self.order
            .extend_from_slice(&nodes[..nodes.len().min(room)]);
        // Truncating leaves a prefix of the namespace rather than the whole of it, so whatever the
        // pager said about running out of entries no longer describes what is held.
        if nodes.len() <= room {
            self.complete |= hit_end;
        }
    }

    /// Drop the enumeration order, an entry having been added or removed. Names stay valid:
    /// binding one name does not change what another resolves to.
    ///
    /// Every external mutation reaches here (`invalidate_name` and `clear` both call it, after the
    /// store call they describe), which makes it the one place the path memo above needs to hear
    /// about them.
    fn invalidate_order(&mut self) {
        self.order.clear();
        self.complete = false;
        self.generation = self.generation.wrapping_add(1);
        super::invalidate_memo();
    }

    /// Drop everything known about `name` as well, in either direction. ext4 reuses inode numbers,
    /// so an unlinked name must not keep resolving out of the cache to what is now a different
    /// file; a name just created must not stay known-absent.
    fn invalidate_name(&mut self, name: &str) {
        self.by_name.pop(name);
        self.absent.pop(name);
        self.invalidate_order();
    }

    fn clear(&mut self) {
        self.by_name.clear();
        self.absent.clear();
        self.invalidate_order();
    }
}

impl ExtNamespace {
    fn cache(&self) -> MutexGuard<'_, NsCache> {
        self.cache.lock().unwrap()
    }
}

impl Namespace for ExtNamespace {
    fn open(id: ObjID, _persist: bool, parent_info: Option<ParentInfo>) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self {
            id,
            parent_info,
            cache: GLOBAL_CACHE.get_namespace_cache(id),
        })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        tracing::debug!("looking up {} in external namespace {}", name, self.id);
        {
            let mut cache = self.cache();
            if let Some(node) = cache.lookup(name) {
                return Some(node);
            }
            if cache.known_absent(name) {
                return None;
            }
        }
        // Note that a miss falls through even when `complete` is set: completeness is only ever a
        // statement about enumeration order at the time of the listing. The pager writes into this
        // tree itself (object-store keeps its per-object files under `ids/`), so a name we have
        // not seen may still exist. Only a lookup that the pager itself answered with NotFound
        // gets to say a name is absent, and only for `NEGATIVE_TTL`.
        let mut guard = pager_handle();
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return None;
        };
        let res = h.lookup_external(self.id, name);

        let file = match res {
            Ok(file) => file,
            Err(err) => {
                // Only a definite absence is worth remembering. A pager that is unreachable, or a
                // device error on the way to the directory, says nothing about whether the name
                // exists, and caching that as absence would turn a transient fault into a
                // sticky ENOENT.
                if err == TwzError::NOT_FOUND {
                    self.cache().cache_absent(name);
                }
                return None;
            }
        };
        let name = file.name()?;
        tracing::trace!(
            "found {} in external namespace {} with ID {} and kind {:?}",
            name,
            self.id,
            file.id,
            file.kind
        );
        let node = match file.kind {
            ExternalKind::SymLink => h
                .readlink_external(file.id.into())
                .and_then(|lname| NsNode::symlink(name, lname)),
            ExternalKind::Directory => NsNode::ns(name, file.id.into()),
            _ => NsNode::obj(name, file.id.into()),
        }
        .ok()?;
        drop(guard);

        self.cache().cache_node(node);
        Some(node)
    }

    fn is_external(&self) -> bool {
        true
    }

    fn create_file(&self, name: &str) -> Result<NsNode> {
        let mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IRGRP | libc::S_IROTH | libc::S_IFREG;
        let mut guard = pager_handle();
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return Err(TwzError::NOT_SUPPORTED);
        };
        let file = h.create_external_file(self.id, name, None, mode)?;
        drop(guard);

        let node = NsNode::obj(name, file.id.into())?;
        let mut cache = self.cache();
        cache.invalidate_order();
        cache.cache_node(node);
        Ok(node)
    }

    fn create_ns(&self, name: &str) -> Result<Option<NsNode>> {
        let mode = libc::S_IRUSR
            | libc::S_IWUSR
            | libc::S_IXUSR
            | libc::S_IRGRP
            | libc::S_IXGRP
            | libc::S_IROTH
            | libc::S_IXOTH
            | libc::S_IFDIR;
        let mut guard = pager_handle();
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return Err(TwzError::NOT_SUPPORTED);
        };
        // The store makes the directory and its ino-derived id is the namespace id, mirroring
        // `create_file` -- no native object is created, so nothing needs binding into the store.
        let file = h.create_external_file(self.id, name, None, mode)?;
        drop(guard);

        let node = NsNode::ns(name, file.id.into())?;
        let mut cache = self.cache();
        cache.invalidate_order();
        cache.cache_node(node);
        Ok(Some(node))
    }

    fn insert(&self, node: NsNode) -> Result<()> {
        tracing::debug!(
            "inserting {} into external namespace {}, id = {}",
            node.name()?,
            self.id,
            node.id
        );
        let mut mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IRGRP | libc::S_IROTH;
        match node.kind {
            NsNodeKind::Namespace => mode |= libc::S_IFDIR,
            NsNodeKind::SymLink => mode |= libc::S_IFLNK,
            NsNodeKind::Object => mode |= libc::S_IFREG,
            // Synthesized by the runtime; there is nothing for a store to bind.
            NsNodeKind::DevNode => return Err(TwzError::NOT_SUPPORTED),
        }

        let mut guard = pager_handle();
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return Err(TwzError::NOT_SUPPORTED);
        };

        // A native ObjID is not an ino, so there is nothing for the external store to bind it to.
        // Creating the file anyway would make an unrelated empty one and silently drop the
        // caller's id, leaving a later `get` of this name to hand back the wrong object.
        if objid_to_ino(node.id.raw()).is_none() {
            return Err(TwzError::NOT_SUPPORTED);
        }
        h.create_external_file(self.id, node.name()?, Some(node.id.into()), mode)?;
        drop(guard);

        // The store, not us, decides what the name ends up bound to, so forget it and let the next
        // lookup ask rather than caching a binding we have not seen confirmed.
        self.cache().invalidate_name(node.name()?);
        Ok(())
    }

    /// External namespaces have no atomic replace of their own: the store's create call is what
    /// defines overwrite semantics here.
    fn replace(&self, node: NsNode) -> Result<()> {
        self.insert(node)
    }

    fn remove(&self, name: &str) -> Result<NsNode> {
        tracing::debug!(
            "removing {} from external namespace {}, id = {}",
            name,
            self.id,
            self.id
        );
        let node = self.find(name).ok_or(NamingError::NotFound)?;
        let mut guard = pager_handle();
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return Err(TwzError::NOT_SUPPORTED);
        };
        // The store's error is the answer, not just a failure signal: removing a non-empty
        // directory comes back as `NotEmpty`, and flattening that to `NotFound` would have
        // libstd's `remove_dir_all` ignore it and report a directory it did not remove.
        h.unlink_external(self.id, name).inspect_err(|e| {
            tracing::warn!(
                "failed to unlink external file {} in namespace {}: {}",
                name,
                self.id,
                e
            )
        })?;
        drop(guard);

        self.cache().invalidate_name(name);
        if node.kind == NsNodeKind::Namespace {
            GLOBAL_CACHE.forget(node.id);
        }
        Ok(node)
    }

    fn id(&self) -> ObjID {
        self.id
    }

    fn persist(&self) -> bool {
        true
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn items(&self, skip: usize, count: usize) -> Vec<NsNode> {
        tracing::debug!(
            "enumerating external namespace {} (skip {}, count {})",
            self.id,
            skip,
            count,
        );
        let t_cached = itemstats::t0();
        let (want, generation) = {
            let cache = self.cache();
            if let Some(items) = cache.window(skip, count) {
                itemstats::record_cached(itemstats::ns(t_cached));
                return items;
            }
            // Extending the known prefix: over-read, so a readdir walking this namespace in small
            // chunks pays one pager call per window instead of one per chunk. Anywhere else, ask
            // for exactly what was requested -- the result cannot join the prefix anyway.
            let want = if skip == cache.order.len() && cache.order_has_room() {
                count.max(PREFETCH)
            } else {
                count
            };
            (want, cache.generation)
        };

        let t_handle = itemstats::t0();
        let mut guard = pager_handle();
        let handle_ns = itemstats::ns(t_handle);
        let Some(h) = guard.as_mut() else {
            tracing::warn!("failed to open handle to pager");
            return vec![];
        };

        let mut entries = Vec::new();
        let t_enum = itemstats::t0();
        let res = h.enumerate_external(self.id, &mut entries, skip, want);
        let enum_ns = itemstats::ns(t_enum);
        if res.is_err() {
            tracing::warn!("failed to enumerate external namespace {}", self.id);
            return vec![];
        }

        // A symlink we have already resolved costs another gate call to resolve again. Take the
        // cache briefly rather than across the readlinks below.
        let known: Vec<Option<NsNode>> = {
            let mut cache = self.cache();
            entries
                .iter()
                .map(|i| {
                    i.name()
                        .and_then(|name| cache.lookup(name))
                        .filter(|node| node.kind == NsNodeKind::SymLink)
                })
                .collect()
        };

        let t_conv = itemstats::t0();
        let mut nr_links = 0u64;
        let mut link_ns = 0u64;
        let mut out: Vec<NsNode> = entries
            .iter()
            .zip(known)
            .filter_map(|(i, known)| {
                let Some(name) = i.name() else {
                    tracing::warn!(
                        "dropping unnamed entry (id {:x}) from external namespace {}",
                        i.id,
                        self.id
                    );
                    return None;
                };
                tracing::trace!(
                    "enumerated {} in external namespace {} with ID {} and kind {:?}",
                    name,
                    self.id,
                    i.id,
                    i.kind
                );
                let node = match i.kind {
                    ExternalKind::Directory => NsNode::ns(name, i.id.into()).ok(),
                    ExternalKind::SymLink => known.or_else(|| {
                        nr_links += 1;
                        let t_link = itemstats::t0();
                        let link = h.readlink_external(i.id.into());
                        link_ns += itemstats::ns(t_link);
                        // A target we cannot read, or cannot store in a node, must not cost the
                        // entry its place in the listing. The caller advances its readdir cursor
                        // by the number of entries it received, while the pager's `skip` counts
                        // what it handed out, so dropping one here slides the window and the tail
                        // of the namespace comes back again in the next chunk. Report it as a
                        // plain object instead: a listing is mostly about names, and this keeps
                        // every one of them.
                        link.inspect_err(|err| {
                            tracing::warn!(
                                "failed to readlink for {} in external namespace {}: {}",
                                name,
                                self.id,
                                err
                            )
                        })
                        .and_then(|lname| {
                            NsNode::symlink(name, &lname).inspect_err(|err| {
                                tracing::warn!(
                                    "{} in external namespace {} has a {}-byte target, too long \
                                     for a node: {}",
                                    name,
                                    self.id,
                                    lname.len(),
                                    err
                                )
                            })
                        })
                        .or_else(|_| NsNode::obj(name, i.id.into()))
                        .ok()
                    }),
                    _ => NsNode::obj(name, i.id.into()).ok(),
                };
                node
            })
            .collect();
        itemstats::record_pager(
            handle_ns,
            enum_ns,
            itemstats::ns(t_conv).saturating_sub(link_ns),
            link_ns,
            out.len() as u64,
            nr_links,
        );
        drop(guard);

        {
            let mut cache = self.cache();
            // A mutation raced this fetch, so the window it describes is already gone.
            if cache.generation == generation {
                // Short of what we asked for means the pager ran out of entries: it applies skip
                // and count after its own filtering, so a short window is the end of the
                // namespace and not a hole.
                cache.record(skip, &out, entries.len() < want, out.len() == entries.len());
            }
        }

        out.truncate(count);
        out
    }
}

// Temporary instrumentation for the directory-enumeration latency hunt (pagerperf.md). An external
// namespace serves an enumeration either out of its cache or from the pager, and in the latter case
// pays one more gate call per symlink to read its target.
mod itemstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Master switch for the *clock reads*, as opposed to the counters.
    ///
    /// `statcadence::report_now` gates the output and leaves the work: four `Instant::now()` per
    /// external enumerate ran on every call regardless. This is the same const-gate `nsidstats` in
    /// `store.rs` already uses, and for the reason its comment gives. Off by default, and when off
    /// nothing is logged rather than logging zeros -- a reader should see silence, not mistake an
    /// ungated build's zeros for measurements.
    pub const TIMING: bool = false;

    /// `Instant::now()`, if [`TIMING`] is on.
    #[inline(always)]
    pub fn t0() -> Option<std::time::Instant> {
        TIMING.then(std::time::Instant::now)
    }

    /// Nanoseconds since `t`, or 0 when [`TIMING`] is off.
    #[inline(always)]
    pub fn ns(t: Option<std::time::Instant>) -> u64 {
        t.map_or(0, |t| t.elapsed().as_nanos() as u64)
    }

    static CACHED: AtomicU64 = AtomicU64::new(0);
    static CACHED_NS: AtomicU64 = AtomicU64::new(0);
    static PAGED: AtomicU64 = AtomicU64::new(0);
    static HANDLE: AtomicU64 = AtomicU64::new(0);
    static ENUM: AtomicU64 = AtomicU64::new(0);
    static CONV: AtomicU64 = AtomicU64::new(0);
    static LINKS: AtomicU64 = AtomicU64::new(0);
    static LINK_NS: AtomicU64 = AtomicU64::new(0);
    static ENTRIES: AtomicU64 = AtomicU64::new(0);

    pub fn record_cached(ns: u64) {
        let n = CACHED.fetch_add(1, Ordering::Relaxed) + 1;
        let c = CACHED_NS.fetch_add(ns, Ordering::Relaxed) + ns;
        if TIMING && secgate::statcadence::report_now(n) {
            secgate::statline!("ITEMSTATS {} cached enumerates: {} us", n, c / 1000);
        }
    }

    pub fn record_pager(handle: u64, enum_ns: u64, conv: u64, link: u64, entries: u64, links: u64) {
        let n = PAGED.fetch_add(1, Ordering::Relaxed) + 1;
        let h = HANDLE.fetch_add(handle, Ordering::Relaxed) + handle;
        let e = ENUM.fetch_add(enum_ns, Ordering::Relaxed) + enum_ns;
        let c = CONV.fetch_add(conv, Ordering::Relaxed) + conv;
        let l = LINK_NS.fetch_add(link, Ordering::Relaxed) + link;
        let nl = LINKS.fetch_add(links, Ordering::Relaxed) + links;
        let ne = ENTRIES.fetch_add(entries, Ordering::Relaxed) + entries;
        if TIMING && secgate::statcadence::report_now(n) {
            secgate::statline!(
                "ITEMSTATS {} pager enumerates, {} entries: handle {} us, enumerate {} us, \
                 convert {} us, readlink {} us over {} links",
                n,
                ne,
                h / 1000,
                e / 1000,
                c / 1000,
                l / 1000,
                nl,
            );
        }
    }
}
