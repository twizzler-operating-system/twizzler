use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use pager_dynamic::{objid_to_ino, ExternalKind};
use twizzler::object::ObjID;

use super::{Namespace, NsNode, ParentInfo};
use crate::{NsNodeKind, Result};

#[derive(Clone)]
pub struct ExtNamespace {
    id: ObjID,
    parent_info: Option<ParentInfo>,
    cache: Arc<Mutex<NsCache>>,
}

struct NsCache {
    cache: BTreeMap<String, NsNode>,
    cache_ready: bool,
}

struct GlobalCache {
    namespaces: Mutex<BTreeMap<ObjID, Arc<Mutex<NsCache>>>>,
}

impl GlobalCache {
    fn get_namespace_cache(&self, id: ObjID) -> Arc<Mutex<NsCache>> {
        let mut namespaces = self.namespaces.lock().unwrap();
        namespaces
            .entry(id)
            .or_insert_with(|| {
                Arc::new(Mutex::new(NsCache {
                    cache: BTreeMap::new(),
                    cache_ready: false,
                }))
            })
            .clone()
    }
}

static GLOBAL_CACHE: GlobalCache = GlobalCache {
    namespaces: Mutex::new(BTreeMap::new()),
};

impl NsCache {
    pub fn cache_ready(&self) -> bool {
        self.cache_ready
    }

    pub fn reset_cache(&mut self) {
        self.cache.clear();
        self.cache_ready = false;
    }

    pub fn cache_node(&mut self, node: NsNode) {
        self.cache.insert(node.name().unwrap().to_string(), node);
    }

    pub fn lookup_cache(&self, name: &str) -> Option<NsNode> {
        self.cache.get(name).cloned()
    }

    pub fn enumerate_cache(&self, skip: usize, count: usize) -> Vec<NsNode> {
        self.cache
            .values()
            .skip(skip)
            .take(count)
            .cloned()
            .collect()
    }

    pub fn load_cache(&mut self, items: impl IntoIterator<Item = NsNode>) {
        for node in items {
            self.cache_node(node);
        }
        self.cache_ready = true;
    }
}

impl ExtNamespace {
    pub fn lookup_cache(&self, name: &str) -> Option<NsNode> {
        self.cache.lock().unwrap().lookup_cache(name)
    }

    pub fn cache_node(&self, node: NsNode) {
        self.cache.lock().unwrap().cache_node(node);
    }

    pub fn enumerate_cache(&self, skip: usize, count: usize) -> Vec<NsNode> {
        self.cache.lock().unwrap().enumerate_cache(skip, count)
    }

    pub fn load_cache(&self) {
        let items = self.items(0, usize::MAX);
        let mut cache = self.cache.lock().unwrap();
        cache.load_cache(items);
    }

    pub fn cache_ready(&self) -> bool {
        self.cache.lock().unwrap().cache_ready()
    }

    pub fn reset_cache(&self) {
        self.cache.lock().unwrap().reset_cache();
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
        if let Some(node) = self.lookup_cache(name) {
            return Some(node);
        }
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

                    if let Ok(node) = node {
                        self.cache_node(node);
                    }

                    node.ok()
                })
            })
        } else {
            None
        }
    }

    fn insert(&self, mut node: NsNode) -> Option<NsNode> {
        tracing::debug!(
            "inserting {} into external namespace {}, id = {}",
            node.name().ok()?,
            self.id,
            node.id
        );
        let mut mode = libc::S_IRUSR | libc::S_IWUSR | libc::S_IRGRP | libc::S_IROTH;
        match node.kind {
            NsNodeKind::Namespace => mode |= libc::S_IFDIR,
            NsNodeKind::SymLink => mode |= libc::S_IFLNK,
            NsNodeKind::Object => mode |= libc::S_IFREG,
        }

        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if objid_to_ino(node.id.raw()).is_none() {
                if let Ok(file) = h.create_external_file(self.id, node.name().ok()?, None, mode) {
                    node.id = file.id.into();
                    if self.cache_ready() {
                        self.reset_cache();
                    }
                    return Some(node);
                } else {
                    tracing::warn!(
                        "failed to create external file {} in namespace {}",
                        node.name().ok()?,
                        self.id
                    );
                }
            } else {
                h.create_external_file(self.id, node.name().ok()?, Some(node.id.into()), mode)
                    .ok()?;
                if self.cache_ready() {
                    self.reset_cache();
                }
                return Some(node);
            }
        } else {
            tracing::warn!("failed to open handle to pager");
        }
        None
    }

    fn remove(&self, name: &str) -> Option<NsNode> {
        tracing::debug!(
            "removing {} from external namespace {}, id = {}",
            name,
            self.id,
            self.id
        );
        let node = self.find(name)?;
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if h.unlink_external(self.id, name).is_ok() {
                self.reset_cache();
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
        tracing::debug!(
            "enumerating external namespace {} (skip {}, count {}, cache-ready {})",
            self.id,
            skip,
            count,
            self.cache_ready(),
        );
        if self.cache_ready() {
            return self.enumerate_cache(skip, count);
        }
        if skip == 0 && count > 60 && count != usize::MAX {
            self.reset_cache();
            self.load_cache();
            tracing::debug!(
                "loaded cache for external namespace {}, now cache-ready = {}",
                self.id,
                self.cache_ready()
            );
            return self.enumerate_cache(skip, count);
        }
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
