use std::{
    io::ErrorKind,
    sync::{Arc, Mutex},
};

use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    object::{ObjID, Object, ObjectBuilder},
};
use twizzler_rt_abi::object::MapFlags;

use super::{Namespace, NsNode, NsNodeKind};
use crate::Result;

#[derive(Clone)]
pub struct NamespaceObject {
    persist: bool,
    obj: Arc<Mutex<Option<VecObject<NsNode, VecObjectAlloc>>>>,
}

impl NamespaceObject {
    pub fn new(persist: bool, parent: Option<ObjID>) -> Result<Self> {
        let mut builder = ObjectBuilder::default();
        if persist {
            builder = builder.persist();
        }
        let mut this = Self {
            persist,
            obj: Arc::new(Mutex::new(Some(
                VecObject::new(builder).map_err(|_| ErrorKind::Other)?,
            ))),
        };
        if let Some(id) = parent {
            this.insert(NsNode::new(NsNodeKind::Namespace, id, "..")?);
        }
        this.insert(NsNode::new(NsNodeKind::Namespace, this.id(), ".")?);
        Ok(this)
    }

    fn with_obj<R>(&self, f: impl FnOnce(&mut VecObject<NsNode, VecObjectAlloc>) -> R) -> R {
        //self.update();
        let mut g = self.obj.lock().unwrap();
        f(g.as_mut().unwrap())
    }

    fn replace_obj(
        &self,
        f: impl FnOnce(VecObject<NsNode, VecObjectAlloc>) -> VecObject<NsNode, VecObjectAlloc>,
    ) {
        let mut g = self.obj.lock().unwrap();
        *g = Some(f(g.take().unwrap()))
    }

    fn update(&self) {
        // TODO: this unwrap is bad.
        self.replace_obj(|obj| VecObject::from(obj.into_object().update().unwrap()));
    }
}

impl Namespace for NamespaceObject {
    fn open(id: ObjID, persist: bool) -> Result<Self> {
        let mut map_flags = MapFlags::READ | MapFlags::WRITE;
        if persist {
            map_flags.insert(MapFlags::PERSIST);
        }
        Ok(Self {
            persist,
            obj: Arc::new(Mutex::new(Some(VecObject::from(
                Object::map(id, map_flags).map_err(|_| ErrorKind::Other)?,
            )))),
        })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        self.with_obj(|obj| {
            for entry in obj.iter() {
                if entry.name.as_str() == name {
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
                if entry.name.as_str() == name {
                    obj.remove(idx).unwrap();
                    return Some(entry);
                }
            }
            None
        })
    }

    fn parent_id(&self) -> Option<ObjID> {
        self.find("..").map(|n| n.id)
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
