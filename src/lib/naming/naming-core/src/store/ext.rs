use twizzler::object::ObjID;

use super::{Namespace, NsNode, NsNodeKind};
use crate::Result;

#[derive(Clone)]
pub struct ExtNamespace {
    id: ObjID,
}

impl Namespace for ExtNamespace {
    fn open(id: ObjID, _persist: bool) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self { id })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        self.items().into_iter().find(|i| i.name() == name)
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

    fn items(&self) -> Vec<NsNode> {
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if let Ok(items) = h.enumerate_external(self.id) {
                return items
                    .iter()
                    .filter_map(|i| {
                        let kind = match i.kind {
                            ExternalKind::Regular => NsNodeKind::Object,
                            ExternalKind::Directory => NsNodeKind::Namespace,
                            _ => NsNodeKind::Object,
                        };
                        i.name()
                            .and_then(|name| NsNode::new(kind, i.id.into(), name).ok())
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
