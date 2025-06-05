use core::str;
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use bitflags::bitflags;
use ext::ExtNamespace;
use nsobj::NamespaceObject;
use object_store::objid_to_ino;
use twizzler::marker::Invariant;
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, NamingError},
    object::ObjID,
};

use crate::{Result, MAX_KEY_SIZE};

mod ext;
mod nsobj;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum NsNodeKind {
    Namespace,
    Object,
    SymLink,
}
unsafe impl Invariant for NsNodeKind {}

const NSID_EXTERNAL: ObjID = ObjID::new(1);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct NsNode {
    name: [u8; MAX_KEY_SIZE],
    pub id: ObjID,
    pub kind: NsNodeKind,
    name_len: u32,
    link_len: u32,
}
unsafe impl Invariant for NsNode {}

impl NsNode {
    pub fn new<P: AsRef<Path>, L: AsRef<Path>>(
        kind: NsNodeKind,
        id: ObjID,
        name: P,
        link_name: Option<L>,
    ) -> Result<Self> {
        let name = name.as_ref().as_os_str().as_encoded_bytes();
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

    fn insert(&self, node: NsNode) -> Option<NsNode>;

    fn remove(&self, name: &str) -> Option<NsNode>;

    fn parent(&self) -> Option<&ParentInfo>;

    fn id(&self) -> ObjID;

    fn items(&self) -> Vec<NsNode>;

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.items().len()
    }

    fn persist(&self) -> bool;
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
            .insert(NsNode::ns("ext", NSID_EXTERNAL).unwrap());
        this
    }

    // Loads in an existing object store from an Object ID
    pub fn new_with(id: ObjID) -> Result<NameStore> {
        let mut this = Self::new();
        this.nameroot.insert(NsNode::ns("data", id).unwrap());
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
        nr_derefs: usize,
        deref: bool,
    ) -> Result<(std::result::Result<NsNode, PathBuf>, Arc<dyn Namespace>)> {
        tracing::trace!("namei: {:?}", name.as_ref());

        let mut namespace = namespace.unwrap_or_else(|| {
            self.working_ns
                .as_ref()
                .unwrap_or(&self.store.nameroot)
                .clone()
        });

        let components = name.as_ref().components().collect::<Vec<_>>();
        if components.is_empty() {
            return Ok((Err("".into()), namespace));
        }

        let mut node = None;
        for (idx, item) in components.iter().enumerate() {
            let is_last = idx == components.len() - 1;
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
                        "lookup component {:?}: {}",
                        os_str.as_encoded_bytes(),
                        os_str.to_str().ok_or(ArgumentError::InvalidArgument)?
                    );
                    node = namespace.find(os_str.to_str().ok_or(ArgumentError::InvalidArgument)?);

                    // Did we find something?
                    let Some(thisnode) = node else {
                        tracing::trace!("failed to find component: (is_last = {})", is_last);
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
                            let ldname = thisnode.readlink()?;
                            tracing::trace!("search with: {}", ldname);
                            let (lnode, lcont) =
                                self.namei_exist(Some(namespace), ldname, nr_derefs - 1, deref)?;
                            node = Some(lnode);
                            namespace = lcont;
                        }
                    }
                    if !is_last && thisnode.kind == NsNodeKind::Namespace {
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

        if let Some(node) = node {
            Ok((Ok(node), namespace))
        } else {
            // Unwrap-Ok: we checked if it's empty earlier.
            Ok((
                Err(components.last().unwrap().as_os_str().into()),
                namespace,
            ))
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
        container.insert(NsNode::ns(name, ns.id())?);
        Ok(())
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, id: ObjID) -> Result<()> {
        tracing::debug!("put {:?}: {}", name.as_ref(), id);
        let (node, container) = self.namei(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        let Err(name) = node else {
            return Err(NamingError::AlreadyExists.into());
        };

        container.insert(NsNode::obj(name, id)?);
        Ok(())
    }

    pub fn get<P: AsRef<Path>>(&self, name: P, flags: GetFlags) -> Result<NsNode> {
        tracing::debug!("get {:?}: {:?}", name.as_ref(), flags);
        let (node, _) = self.namei_exist(
            None,
            name,
            Self::MAX_SYMLINK_DEREF,
            flags.contains(GetFlags::FOLLOW_SYMLINK),
        )?;
        Ok(node)
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> Result<std::vec::Vec<NsNode>> {
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
        let items = ns.items();
        tracing::trace!("collected: {:?}", items);
        Ok(items)
    }

    pub fn enumerate_namespace_nsid(&self, id: ObjID) -> Result<std::vec::Vec<NsNode>> {
        tracing::trace!("opening namespace-ensid: {}", id);
        let ns = self.open_namespace(id, false, None)?;
        let items = ns.items();
        tracing::trace!("collected: {:?}", items);
        Ok(items)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        tracing::debug!("change_ns: {:?}", name.as_ref());
        let (node, container) = self.namei_exist(None, name, Self::MAX_SYMLINK_DEREF, true)?;
        match node.kind {
            NsNodeKind::Namespace => {
                self.working_ns = Some(self.open_namespace(
                    node.id,
                    container.persist(),
                    Some(ParentInfo::new(container, node.name()?)),
                )?);
                Ok(())
            }
            _ => Err(NamingError::WrongNameKind.into()),
        }
    }

    pub fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        let (node, container) = self.namei_exist(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        container
            .remove(node.name()?)
            .map(|_| ())
            .ok_or(NamingError::NotFound.into())
    }

    pub fn link<P: AsRef<Path>, L: AsRef<Path>>(&self, name: P, link: L) -> Result<()> {
        let (node, container) = self.namei(None, &name, Self::MAX_SYMLINK_DEREF, false)?;
        let Err(name) = node else {
            return Err(NamingError::AlreadyExists.into());
        };

        container.insert(NsNode::symlink(name, link)?);
        Ok(())
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
    }
}
