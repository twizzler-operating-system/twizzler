use pager_dynamic::ExternalKind;
use twizzler::object::ObjID;

use super::{Namespace, NsNode, ParentInfo};
use crate::{NsNodeKind, Result};

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
        tracing::trace!("looking up {} in external namespace {}", name, self.id);
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            h.lookup_external(self.id, name).ok().and_then(|i| {
                tracing::trace!(
                    "found {} in external namespace {} with ID {} and kind {:?}",
                    name,
                    self.id,
                    i.id,
                    i.kind
                );
                i.name().and_then(|name| {
                    tracing::trace!(
                        "creating node for {} in external namespace {} with ID {} and kind {:?}",
                        name,
                        self.id,
                        i.id,
                        i.kind
                    );
                    let node = match i.kind {
                        ExternalKind::SymLink => h
                            .readlink_external(i.id.into())
                            .and_then(|lname| NsNode::symlink(name, lname)),
                        ExternalKind::Directory => NsNode::ns(name, i.id.into()),
                        _ => NsNode::obj(name, i.id.into()),
                    };

                    node.ok()
                })
            })
        } else {
            None
        }
    }

    fn insert(&self, mut node: NsNode) -> Option<NsNode> {
        let mut mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IRGRP | libc::S_IROTH;
        match node.kind {
            NsNodeKind::Namespace => mode |= libc::S_IFDIR,
            NsNodeKind::SymLink => mode |= libc::S_IFLNK,
            NsNodeKind::Object => mode |= libc::S_IFREG,
            _ => {}
        }

        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if let Ok(file) = h.create_external_file(self.id, node.name().ok()?, mode) {
                node.id = file.id.into();
                return Some(node);
            } else {
                tracing::warn!(
                    "failed to create external file {} in namespace {}",
                    node.name().ok()?,
                    self.id
                );
            }
        } else {
            tracing::warn!("failed to open handle to pager");
        }
        None
    }

    fn remove(&self, name: &str) -> Option<NsNode> {
        let node = self.find(name)?;
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if h.unlink_external(self.id, name).is_ok() {
                return Some(node);
            } else {
                tracing::warn!(
                    "failed to unlink external file {} in namespace {}",
                    name,
                    self.id
                );
            }
        } else {
            tracing::warn!("failed to open handle to pager");
        }
        None
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
        tracing::trace!(
            "enumerating external namespace {} (skip {}, count {})",
            self.id,
            skip,
            count
        );
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            let mut entries = Vec::new();
            if let Ok(_) = h.enumerate_external(self.id, &mut entries, skip, count) {
                return entries
                    .iter()
                    .filter_map(|i| {
                        i.name().and_then(|name| {
                            tracing::trace!(
                                "enumerated {} in external namespace {} with ID {} and kind {:?}",
                                name,
                                self.id,
                                i.id,
                                i.kind
                            );
                            match i.kind {
                                ExternalKind::Directory => NsNode::ns(name, i.id.into()),
                                ExternalKind::SymLink => {
                                    if let Ok(lname) = h.readlink_external(i.id.into()) {
                                        NsNode::symlink(name, lname)
                                    } else {
                                        tracing::warn!(
                                            "failed to readlink for {} in external namespace {}",
                                            name,
                                            self.id
                                        );
                                        NsNode::obj(name, i.id.into())
                                    }
                                }
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
