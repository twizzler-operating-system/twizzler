use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
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

/// One lock per namespace *object*, shared by every `NamespaceObject` instance over it.
///
/// `open` maps the object afresh on each call, so two instances of one namespace are two mappings
/// of the same memory behind two unrelated `Mutex`es. `VecObject`'s length lives in that shared
/// memory and `push` is a read-modify-write of it, so without a lock keyed on the object -- not on
/// the instance -- two gate calls inserting into one namespace can lose an entry outright.
static OBJ_LOCKS: RwLock<BTreeMap<ObjID, Arc<RwLock<()>>>> = RwLock::new(BTreeMap::new());

fn obj_lock(id: ObjID) -> Arc<RwLock<()>> {
    // The hit is the whole workload once a namespace has been seen once, and it needs no mutation
    // of the map -- so take the read guard first and only fall back to the writer on a miss.
    if let Some(lock) = OBJ_LOCKS.read().unwrap().get(&id) {
        return lock.clone();
    }
    OBJ_LOCKS
        .write()
        .unwrap()
        .entry(id)
        .or_insert_with(|| Arc::new(RwLock::new(())))
        .clone()
}

fn find_idx(obj: &VecObject<NsNode, VecObjectAlloc>, name: &str) -> Option<usize> {
    obj.iter().position(|e| e.name().is_ok_and(|n| n == name))
}

#[derive(Clone)]
pub struct NamespaceObject {
    persist: bool,
    id: ObjID,
    obj: Arc<RwLock<Option<VecObject<NsNode, VecObjectAlloc>>>>,
    lock: Arc<RwLock<()>>,
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
        let this = Self {
            persist,
            id,
            parent_info,
            obj: Arc::new(RwLock::new(Some(vec))),
            lock: obj_lock(id),
        };
        if let Some(parent) = parent {
            this.insert(NsNode::ns("..", parent)?)?;
        }
        this.insert(NsNode::ns(".", id)?)?;
        Ok(this)
    }

    /// Shared lock first, always. `f` must not re-enter `with_obj`: neither lock is reentrant.
    fn with_obj<R>(&self, f: impl FnOnce(&mut VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        let _shared = self.lock.write().unwrap();
        let mut g = self.obj.write().unwrap();
        f(g.as_mut().unwrap())
    }

    /// The read-only half of [`Self::with_obj`], for the lookups that only iterate.
    ///
    /// Same order and the same non-reentrancy rule. This exists so `find`/`items`/`len` -- which
    /// are the whole of a namei walk -- do not serialize against each other.
    fn with_obj_ref<R>(&self, f: impl FnOnce(&VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        let _shared = self.lock.read().unwrap();
        let g = self.obj.read().unwrap();
        f(g.as_ref().unwrap())
    }
}

impl Namespace for NamespaceObject {
    fn open(id: ObjID, persist: bool, parent_info: Option<ParentInfo>) -> Result<Self> {
        let mut map_flags = MapFlags::READ | MapFlags::WRITE;
        if persist {
            map_flags.insert(MapFlags::PERSIST);
        }
        Ok(Self {
            persist,
            id,
            parent_info,
            obj: Arc::new(RwLock::new(Some(VecObject::from(Object::map(
                id, map_flags,
            )?)))),
            lock: obj_lock(id),
        })
    }

    fn create_file(&self, name: &str) -> Result<NsNode> {
        let create = if self.persist {
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
        self.with_obj_ref(|obj| {
            for entry in obj.iter() {
                let Ok(en) = entry.name() else {
                    continue;
                };
                tracing::trace!("compare: {}: {}", en, name);
                if en == name {
                    return Some(*entry);
                }
            }
            None
        })
    }

    fn insert(&self, node: NsNode) -> Result<()> {
        // After the write, never before: a reader that sampled the generation *after* an early
        // bump but walked before the entry landed would record a stale answer as a current one.
        let res = self.with_obj(|obj| {
            if find_idx(obj, node.name()?).is_some() {
                return Err(NamingError::AlreadyExists.into());
            }
            obj.push(node)
        });
        super::invalidate_memo();
        res
    }

    fn replace(&self, node: NsNode) -> Result<()> {
        let res = self.with_obj(|obj| {
            if let Some(idx) = find_idx(obj, node.name()?) {
                obj.remove(idx)?;
            }
            obj.push(node)
        });
        super::invalidate_memo();
        res
    }

    fn remove(&self, name: &str) -> Option<NsNode> {
        let res = self.with_obj(|obj| {
            for (idx, entry) in obj.iter().enumerate() {
                let entry = *entry;
                let Ok(en) = entry.name() else {
                    continue;
                };
                if en == name {
                    obj.remove(idx).unwrap();
                    return Some(entry);
                }
            }
            None
        });
        super::invalidate_memo();
        res
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn id(&self) -> ObjID {
        self.id
    }

    fn len(&self) -> usize {
        self.with_obj_ref(|obj| obj.len())
    }

    fn persist(&self) -> bool {
        self.persist
    }

    fn items(&self, skip: usize, count: usize) -> Vec<NsNode> {
        self.with_obj_ref(|obj| obj.iter().skip(skip).take(count).cloned().collect())
    }
}
