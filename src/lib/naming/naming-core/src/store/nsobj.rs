use std::sync::{Arc, Mutex};

use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjID, Object, ObjectBuilder},
};
use twizzler_rt_abi::object::MapFlags;

use super::{Namespace, NsNode, ParentInfo};
use crate::Result;

#[derive(Clone)]
pub struct NamespaceObject {
    persist: bool,
    obj: Arc<Mutex<Option<VecObject<NsNode, VecObjectAlloc>>>>,
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
            builder = builder.persist();
        }
        let this = Self {
            persist,
            parent_info,
            obj: Arc::new(Mutex::new(Some(VecObject::new(builder)?))),
        };
        if let Some(id) = parent {
            this.insert(NsNode::ns("..", id)?);
        }
        this.insert(NsNode::ns(".", this.id())?);
        Ok(this)
    }

    fn with_obj<R>(&self, f: impl FnOnce(&mut VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        //self.update();
        let mut g = self.obj.lock().unwrap();
        f(g.as_mut().unwrap())
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
            parent_info,
            obj: Arc::new(Mutex::new(Some(VecObject::from(Object::map(
                id, map_flags,
            )?)))),
        })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        self.with_obj(|obj| {
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

    fn insert(&self, node: NsNode) -> Option<NsNode> {
        self.with_obj(|obj| {
            obj.push(node).unwrap();
            None
        })
    }

    fn remove(&self, name: &str) -> Option<NsNode> {
        self.with_obj(|obj| {
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
        })
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn id(&self) -> ObjID {
        self.with_obj(|obj| obj.object().id())
    }

    fn len(&self) -> usize {
        self.with_obj(|obj| obj.len())
    }

    fn persist(&self) -> bool {
        self.persist
    }

    fn items(&self) -> Vec<NsNode> {
        self.with_obj(|obj| obj.iter().cloned().collect())
    }
}
