use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, RwLock, Weak},
};

use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjID, Object, ObjectBuilder},
};
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_rt_abi::{error::NamingError, object::MapFlags};

use super::{Namespace, NsNode, ParentInfo};
use crate::Result;

/// Everything keyed on the namespace *object* rather than on one mapping of it.
///
/// Two mappings of one namespace can exist -- see [`NS_OBJECTS`], which keys on the map flags as
/// well as the id -- so anything that describes the object's *contents* has to live here, or a
/// change made through one mapping would be invisible to the other.
struct NsObjState {
    /// Orders mutations across every mapping of this object. `VecObject`'s length lives in the
    /// object's own memory and `push` is a read-modify-write of it, so without a lock keyed on the
    /// object two gate calls inserting into one namespace can lose an entry outright.
    lock: RwLock<()>,
    /// `name -> position in the vector`, so a lookup is a hash probe instead of a linear scan
    /// with a `str::from_utf8` per entry passed.
    ///
    /// `None` until first built. Built lazily under `lock`, and maintained -- not dropped -- by
    /// the mutation paths: an index rebuilt on every insert would cost O(n) per insert and turn
    /// building a directory back into the O(n^2) this exists to remove.
    ///
    /// Deliberately in memory rather than a second persistent object beside the vector. An index
    /// stored in the object would still have to be built on first open for every namespace that
    /// predates it, and [`NS_OBJECTS`] already makes that once per namespace per boot -- so
    /// persisting it would buy nothing over building it here, at the cost of a second object per
    /// namespace and a format migration.
    index: RwLock<Option<HashMap<Box<str>, usize>>>,
}

/// `Weak`, so a namespace's state -- and with it an index holding a `Box<str>` per entry -- dies
/// with the last mapping of it. The lock this replaces was a bare `Arc<RwLock<()>>` and its
/// unbounded growth cost almost nothing; an index per namespace ever walked, retained for the life
/// of naming-srv, is a different proposition.
///
/// Dropping the state also drops the index, which is what keeps it honest if an `ObjID` is ever
/// reused: there is no live entry to inherit. (`ext.rs::forget` exists for the same hazard, where
/// ext4 really does reuse inode numbers.)
static OBJ_STATE: RwLock<BTreeMap<ObjID, Weak<NsObjState>>> = RwLock::new(BTreeMap::new());

fn obj_state(id: ObjID) -> Arc<NsObjState> {
    // The hit is the whole workload once a namespace has been seen once, and it needs no mutation
    // of the map -- so take the read guard first and only fall back to the writer on a miss.
    if let Some(state) = OBJ_STATE.read().unwrap().get(&id).and_then(Weak::upgrade) {
        return state;
    }
    let mut map = OBJ_STATE.write().unwrap();
    // A racing opener may have won while we upgraded the guard. Theirs is the live one, and it
    // must be: two mappings of one object holding different locks is the tearing this prevents.
    if let Some(state) = map.get(&id).and_then(Weak::upgrade) {
        return state;
    }
    let state = Arc::new(NsObjState {
        lock: RwLock::new(()),
        index: RwLock::new(None),
    });
    map.insert(id, Arc::downgrade(&state));
    if map.len() > NS_OBJECTS_SWEEP {
        // `state` is held here, so the entry just inserted survives this.
        map.retain(|_, w| w.strong_count() > 0);
    }
    state
}

/// `name -> position in the vector`.
type Index = HashMap<Box<str>, usize>;

/// Fix the index up after `VecObject::remove(at)`, which compacts: every entry past `at` moved
/// down one. O(n), but `remove` itself already shifts the same n entries, so this costs the
/// removal path nothing asymptotically and leaves the lookup path O(1).
fn index_removed(index: &mut Index, name: &str, at: usize) {
    index.remove(name);
    for pos in index.values_mut() {
        if *pos > at {
            *pos -= 1;
        }
    }
}

fn build_index(obj: &VecObject<NsNode, VecObjectAlloc>) -> Index {
    let mut map = HashMap::with_capacity(obj.len());
    for (idx, entry) in obj.iter().enumerate() {
        if let Ok(name) = entry.name() {
            // First binding wins, matching the scan this replaces: `find` returned the first
            // match, and a namespace should not contain duplicates anyway.
            map.entry(name.into()).or_insert(idx);
        }
    }
    map
}

/// Live namespace mappings, keyed by object id *and* by the map flags they were opened under.
///
/// `namei` opens a namespace for every non-terminal component of every uncached walk, and opening
/// one used to mean an `Object::map` -- a monitor gate call whenever the runtime's handle cache
/// misses -- plus two `Arc::new`s and a `BTreeMap` lookup, every time. Resolving `/a/b/c/d` paid
/// that four times over. With this, a repeat walk costs an `Arc` clone per component.
///
/// `Weak`, so an entry disappears when the last walk holding it drops. A cache of `Arc`s would pin
/// every namespace mapping ever walked for the life of the process; `ext.rs`'s `GlobalCache`
/// comment is explicit about what that shape costs, and there is no reason to inherit it here.
///
/// Keyed on `persist` too because it decides `MapFlags::PERSIST`: a walk opens a persistent
/// namespace with it, and `enumerate_namespace` opens the same one without it. Two mappings of one
/// object is what the tree already did before this cache existed; `OBJ_LOCKS` is what orders them,
/// and it stays keyed on the id alone.
static NS_OBJECTS: RwLock<BTreeMap<(ObjID, bool), Weak<NsShared>>> = RwLock::new(BTreeMap::new());

/// Sweep dead `Weak`s once the map grows past this. Entries cost a `Weak` each, and nothing else
/// walks this map, so there is no reason to pay for a sweep on every open.
const NS_OBJECTS_SWEEP: usize = 64;

/// The per-*object* half of a namespace: the mapping and the lock ordering access to it.
///
/// Deliberately does not hold `parent_info`. That records how one particular walk *reached* this
/// namespace, and two paths can reach the same namespace under different names -- so it is a
/// property of the walk, not of the object, and sharing one would make `cwd_path` read back
/// whichever name happened to be cached first.
struct NsShared {
    id: ObjID,
    persist: bool,
    obj: RwLock<VecObject<NsNode, VecObjectAlloc>>,
    state: Arc<NsObjState>,
}

impl NsShared {
    fn map(id: ObjID, persist: bool) -> Result<Self> {
        let mut map_flags = MapFlags::READ | MapFlags::WRITE;
        if persist {
            map_flags.insert(MapFlags::PERSIST);
        }
        Ok(Self {
            id,
            persist,
            obj: RwLock::new(VecObject::from(Object::map(id, map_flags)?)),
            state: obj_state(id),
        })
    }

    /// Shared lock first, always. `f` must not re-enter `with_obj`: neither lock is reentrant.
    ///
    /// `f` gets the index alongside the vector and is responsible for keeping the two in step --
    /// every mutation path below does, rather than dropping the index and rebuilding, which would
    /// put an O(n) rebuild on every insert.
    fn with_obj<R>(
        &self,
        f: impl FnOnce(&mut VecObject<NsNode, VecObjectAlloc>, &mut Option<Index>) -> R,
    ) -> R {
        let _shared = self.state.lock.write().unwrap();
        let mut g = self.obj.write().unwrap();
        let mut idx = self.state.index.write().unwrap();
        f(&mut g, &mut idx)
    }

    /// The read-only half of [`Self::with_obj`], for the lookups that only iterate.
    ///
    /// Same order and the same non-reentrancy rule. This exists so `find`/`items`/`len` -- which
    /// are the whole of a namei walk -- do not serialize against each other.
    fn with_obj_ref<R>(&self, f: impl FnOnce(&VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        let _shared = self.state.lock.read().unwrap();
        let g = self.obj.read().unwrap();
        f(&g)
    }

    /// Look a name up through the index, building it on first use.
    ///
    /// Takes only the *read* side of the object lock: the index describes the object's contents,
    /// and no mutation can be in flight while this is held, so building it here cannot race one.
    fn lookup(&self, name: &str) -> Option<NsNode> {
        let _shared = self.state.lock.read().unwrap();
        let obj = self.obj.read().unwrap();
        let mut index = self.state.index.write().unwrap();
        let index = index.get_or_insert_with(|| build_index(&obj));
        let pos = *index.get(name)?;
        // `slice()`, not `nth(pos)`: `VecIter` implements only `next()`, so `nth` would walk the
        // vector and give back the O(n) this index exists to remove. The iterator is bound rather
        // than temporary because it owns the `Ref` guard keeping the mapping resolved -- the
        // slice borrows from that, so it must outlive the read.
        let it = obj.iter();
        it.slice().get(pos).copied()
    }
}

/// The live `NsShared` for `(id, persist)`, mapping the object only if nobody else holds one.
fn ns_shared(id: ObjID, persist: bool) -> Result<Arc<NsShared>> {
    let key = (id, persist);
    if let Some(shared) = NS_OBJECTS.read().unwrap().get(&key).and_then(Weak::upgrade) {
        return Ok(shared);
    }
    let mut map = NS_OBJECTS.write().unwrap();
    // A racing opener may have won while we were upgrading the guard; theirs is the live one.
    if let Some(shared) = map.get(&key).and_then(Weak::upgrade) {
        return Ok(shared);
    }
    let shared = Arc::new(NsShared::map(id, persist)?);
    map.insert(key, Arc::downgrade(&shared));
    if map.len() > NS_OBJECTS_SWEEP {
        // `shared` is held here, so the entry just inserted survives this.
        map.retain(|_, w| w.strong_count() > 0);
    }
    Ok(shared)
}

/// Register an `NsShared` built from a freshly created object, so the next walk to reach it shares
/// this mapping instead of making a second one.
fn ns_publish(shared: &Arc<NsShared>) {
    NS_OBJECTS
        .write()
        .unwrap()
        .insert((shared.id, shared.persist), Arc::downgrade(shared));
}

/// Position of `name`, through the index, building it if this is its first use.
fn find_idx(
    obj: &VecObject<NsNode, VecObjectAlloc>,
    index: &mut Option<Index>,
    name: &str,
) -> Option<usize> {
    index
        .get_or_insert_with(|| build_index(obj))
        .get(name)
        .copied()
}

/// One namespace as reached by one walk: the shared mapping, plus how this walk got here.
///
/// Cloning is an `Arc` clone and a `ParentInfo` clone -- no object map. See [`NsShared`] for the
/// half that is shared and [`NS_OBJECTS`] for what makes it so.
#[derive(Clone)]
pub struct NamespaceObject {
    inner: Arc<NsShared>,
    parent_info: Option<ParentInfo>,
}

impl NamespaceObject {
    pub fn new(
        persist: bool,
        parent: Option<ObjID>,
        parent_info: Option<ParentInfo>,
    ) -> Result<Self> {
        let mut builder = ObjectBuilder::default();
        if persist {
            builder = builder.persist(true);
        }
        let vec = VecObject::new(builder)?;
        let id = vec.object().id();
        let inner = Arc::new(NsShared {
            id,
            persist,
            obj: RwLock::new(vec),
            state: obj_state(id),
        });
        // Publish before the inserts below: they go through `self`, and a concurrent walk that
        // reaches this id must share this mapping rather than make a second one.
        ns_publish(&inner);
        let this = Self { inner, parent_info };
        if let Some(parent) = parent {
            this.insert(NsNode::ns("..", parent)?)?;
        }
        this.insert(NsNode::ns(".", id)?)?;
        Ok(this)
    }

    fn with_obj<R>(
        &self,
        f: impl FnOnce(&mut VecObject<NsNode, VecObjectAlloc>, &mut Option<Index>) -> R,
    ) -> R {
        self.inner.with_obj(f)
    }

    fn with_obj_ref<R>(&self, f: impl FnOnce(&VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        self.inner.with_obj_ref(f)
    }
}

impl Namespace for NamespaceObject {
    fn open(id: ObjID, persist: bool, parent_info: Option<ParentInfo>) -> Result<Self> {
        Ok(Self {
            inner: ns_shared(id, persist)?,
            parent_info,
        })
    }

    fn create_file(&self, name: &str) -> Result<NsNode> {
        let create = if self.inner.persist {
            ObjectCreate {
                flags: ObjectCreateFlags::empty(),
                kuid: 0.into(),
                bt: BackingType::Normal,
                lt: LifetimeType::Persistent,
                def_prot: Protections::all(),
            }
        } else {
            ObjectCreate {
                flags: ObjectCreateFlags::DELETE,
                kuid: 0.into(),
                bt: BackingType::Normal,
                lt: LifetimeType::Volatile,
                def_prot: Protections::all(),
            }
        };

        let id = sys_object_create(
            create,
            &[],
            &[CreateTieSpec::new(self.id(), CreateTieFlags::empty()).into()],
        )?;
        let node = NsNode::obj(name, id)?;
        self.insert(node)?;
        Ok(node)
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        self.inner.lookup(name)
    }

    fn insert(&self, node: NsNode) -> Result<()> {
        // After the write, never before: a reader that sampled the generation *after* an early
        // bump but walked before the entry landed would record a stale answer as a current one.
        let res = self.with_obj(|obj, index| {
            let name = node.name()?;
            if find_idx(obj, index, name).is_some() {
                return Err(NamingError::AlreadyExists.into());
            }
            let at = obj.len();
            let name: Box<str> = name.into();
            obj.push(node)?;
            // Appended, so no other position moved: maintaining the index is one insert.
            if let Some(index) = index {
                index.insert(name, at);
            }
            Ok(())
        });
        // No memo invalidation, deliberately. The path memo caches *only successful* lookups (see
        // `NameSession::get`: "Only a hit is remembered"), and this `insert` cannot rebind a name
        // that already exists -- it returns `AlreadyExists` above. Binding a new name therefore
        // cannot change what any already-cached path resolves to, so retiring the memo here only
        // destroyed entries that were still correct.
        //
        // That mattered: the memo is retired globally, so before this every insert anywhere in the
        // tree emptied all 512 entries. A workload that interleaves creates with lookups -- a
        // build, an unpack, init binding its coreutils symlinks -- kept it permanently cold.
        // `replace` and `remove` below still invalidate, because those two *can* change an
        // existing binding.
        res
    }

    fn replace(&self, node: NsNode) -> Result<()> {
        let res = self.with_obj(|obj, index| {
            let name = node.name()?;
            if let Some(idx) = find_idx(obj, index, name) {
                obj.remove(idx)?;
                if let Some(index) = index {
                    index_removed(index, name, idx);
                }
            }
            let at = obj.len();
            let name: Box<str> = name.into();
            obj.push(node)?;
            if let Some(index) = index {
                index.insert(name, at);
            }
            Ok(())
        });
        super::invalidate_memo();
        res
    }

    fn remove(&self, name: &str) -> Option<NsNode> {
        let res = self.with_obj(|obj, index| {
            let idx = find_idx(obj, index, name)?;
            let entry = obj.remove(idx).ok()?;
            if let Some(index) = index {
                index_removed(index, name, idx);
            }
            Some(entry)
        });
        super::invalidate_memo();
        res
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn id(&self) -> ObjID {
        self.inner.id
    }

    fn len(&self) -> usize {
        self.with_obj_ref(|obj| obj.len())
    }

    fn persist(&self) -> bool {
        self.inner.persist
    }

    fn items(&self, skip: usize, count: usize) -> Vec<NsNode> {
        self.with_obj_ref(|obj| {
            // `slice()`, not `iter().skip(skip)`. `VecIter` implements only `next()`, so `skip`
            // walks the vector one entry at a time -- and a readdir pages through a namespace with
            // a growing `skip`, which made listing n entries cost O(n^2) iterator steps. Bound to
            // a local because the iterator owns the `Ref` guard the slice borrows from.
            let it = obj.iter();
            let all = it.slice();
            let start = skip.min(all.len());
            let end = start.saturating_add(count).min(all.len());
            all[start..end].to_vec()
        })
    }
}
