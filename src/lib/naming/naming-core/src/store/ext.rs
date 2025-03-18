use twizzler::object::ObjID;

use super::{Namespace, NsNode, ParentInfo};
use crate::Result;

#[derive(Clone)]
pub struct ExtNamespace {
    id: ObjID,
    parent_info: Option<ParentInfo>,
}

impl Namespace for ExtNamespace {
    fn open(id: ObjID, _persist: bool, parent_info: Option<ParentInfo>) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self { id, parent_info })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        self.items()
            .into_iter()
            .find(|i| i.name().is_ok_and(|n| n == name))
    }

    fn insert(&self, _node: NsNode) -> Option<NsNode> {
        None
    }

    fn remove(&self, _name: &str) -> Option<NsNode> {
        None
    }

    fn id(&self) -> ObjID {
        self.id
    }

    fn persist(&self) -> bool {
        false
    }

    fn parent(&self) -> Option<&ParentInfo> {
        self.parent_info.as_ref()
    }

    fn items(&self) -> Vec<NsNode> {
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if let Ok(items) = h.enumerate_external(self.id) {
                return items
                    .iter()
                    .filter_map(|i| {
                        i.name().and_then(|name| {
                            match i.kind {
                                ExternalKind::Directory => NsNode::ns(name, i.id.into()),
                                // TODO: symlink
                                _ => NsNode::obj(name, i.id.into()),
                            }
                            .ok()
                        })
                    })
                    .collect();
            } else {
                tracing::warn!("failed to enumerate external namespace {}", self.id);
            }
        } else {
            tracing::warn!("failed to open handle to pager");
        }
        vec![]
    }
}

use pager_dynamic::ExternalKind;
