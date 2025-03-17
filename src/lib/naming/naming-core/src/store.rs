use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use arrayvec::ArrayString;
use ext::ExtNamespace;
use nsobj::NamespaceObject;
use object_store::objid_to_ino;
use twizzler::marker::Invariant;
use twizzler_rt_abi::object::ObjID;

use crate::{Result, MAX_KEY_SIZE};

mod ext;
mod nsobj;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum NsNodeKind {
    Namespace,
    Object,
}
unsafe impl Invariant for NsNodeKind {}

const NSID_EXTERNAL: ObjID = ObjID::new(1);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct NsNode {
    pub kind: NsNodeKind,
    pub id: ObjID,
    name: ArrayString<MAX_KEY_SIZE>,
}
unsafe impl Invariant for NsNode {}

impl NsNode {
    pub fn new<P: AsRef<Path>>(kind: NsNodeKind, id: ObjID, name: P) -> Result<Self> {
        let name = ArrayString::from(name.as_ref().to_str().ok_or(ErrorKind::InvalidFilename)?)
            .map_err(|_| ErrorKind::InvalidFilename)?;
        Ok(Self { kind, id, name })
    }

    pub fn ext(name: ArrayString<MAX_KEY_SIZE>) -> Self {
        Self::ns(name, NSID_EXTERNAL)
    }

    pub fn ns(name: ArrayString<MAX_KEY_SIZE>, id: ObjID) -> Self {
        Self {
            kind: NsNodeKind::Namespace,
            id,
            name,
        }
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
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

    fn len(&self) -> usize {
        self.items().len()
    }

    fn persist(&self) -> bool;
}

pub struct NameStore {
    nameroot: Arc<dyn Namespace>,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let this = NameStore {
            nameroot: Arc::new(NamespaceObject::new(false, None, None).unwrap()),
        };
        this.nameroot
            .insert(NsNode::ext(ArrayString::from("ext").unwrap()));
        this
    }

    // Loads in an existing object store from an Object ID
    pub fn new_with(id: ObjID) -> Result<NameStore> {
        let this = Self::new();
        this.nameroot
            .insert(NsNode::ns(ArrayString::from("data").unwrap(), id));
        tracing::debug!(
            "new_with: root={}, data={:?}",
            id,
            this.nameroot.find("data")
        );
        Ok(this)
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
    fn open_namespace(
        &self,
        id: ObjID,
        persist: bool,
        parent_info: Option<ParentInfo>,
    ) -> Result<Arc<dyn Namespace>> {
        Ok(if id == NSID_EXTERNAL || objid_to_ino(id.raw()).is_some() {
            Arc::new(ExtNamespace::open(id, persist, parent_info)?)
        } else {
            Arc::new(NamespaceObject::open(id, persist, parent_info)?)
        })
    }

    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<P: AsRef<Path>>(
        &self,
        name: P,
    ) -> Result<(Option<NsNode>, Arc<dyn Namespace>, PathBuf)> {
        let (mut namespace, mut node) = if name.as_ref().has_root() {
            (
                self.store.nameroot.clone(),
                Some(NsNode::new(NsNodeKind::Namespace, self.store.nameroot.id(), "/").unwrap()),
            )
        } else {
            let ns = self.working_ns.as_ref().unwrap_or(&self.store.nameroot);
            (
                ns.clone(),
                Some(NsNode::new(NsNodeKind::Namespace, ns.id(), "/").unwrap()),
            )
        };
        if name.as_ref().components().next().is_none() {
            return Ok((None, namespace, "".into()));
        }
        tracing::debug!("namei: {:?}", name.as_ref());
        let mut remname = name.as_ref().to_owned();
        // traverse store based on path's components
        let mut do_traverse = false;
        for item in name.as_ref().components() {
            if do_traverse {
                if let Some(node) = node.take() {
                    if node.kind != NsNodeKind::Namespace {
                        return Err(ErrorKind::NotADirectory);
                    }
                    tracing::debug!("traversing to {} => {}", node.name, node.id);
                    let parent_info = ParentInfo::new(namespace, node.name());
                    namespace =
                        self.open_namespace(node.id, parent_info.ns.persist(), Some(parent_info))?;
                }
            }
            do_traverse = false;
            match item {
                Component::Prefix(_) => {
                    continue;
                }
                Component::RootDir => {
                    tracing::debug!("nameroot clone");
                    namespace = self.store.nameroot.clone();
                    node = Some(NsNode::ns(ArrayString::from("/").unwrap(), namespace.id()));
                    tracing::debug!("again from the top");
                    remname = PathBuf::from("/");
                    continue;
                }
                Component::CurDir => {
                    node = namespace.find(".");
                    remname = PathBuf::from(".");
                }
                Component::ParentDir => {
                    if let Some(parent) = namespace.parent() {
                        node = Some(NsNode::new(
                            NsNodeKind::Namespace,
                            parent.ns.id(),
                            &parent.name_in_parent,
                        )?);
                        namespace = parent.ns.clone();
                        remname = PathBuf::from("..");
                    }
                    continue;
                }
                Component::Normal(os_str) => {
                    tracing::debug!("lookup component {:?}", os_str);
                    node = namespace.find(os_str.to_str().ok_or(ErrorKind::InvalidFilename)?);
                    tracing::debug!("again from the top");
                    remname = PathBuf::from(os_str);
                    do_traverse = true;
                }
            }
        }

        tracing::debug!(
            "namei: {:?} => {:?} in {}",
            name.as_ref(),
            node,
            namespace.id()
        );
        Ok((node, namespace, remname))
    }

    fn namei_exist<'a, P: AsRef<Path>>(&self, name: P) -> Result<(NsNode, Arc<dyn Namespace>)> {
        let (n, ns, _) = self.namei(name)?;
        Ok((n.ok_or(ErrorKind::NotFound)?, ns))
    }

    pub fn mkns<P: AsRef<Path>>(&self, name: P, persist: bool) -> Result<()> {
        let (_node, container, remname) = self.namei(&name)?;
        let ns = NamespaceObject::new(
            persist,
            Some(container.id()),
            Some(ParentInfo::new(
                container.clone(),
                remname.display().to_string(),
            )),
        )?;
        container.insert(NsNode::new(NsNodeKind::Namespace, ns.id(), remname)?);
        Ok(())
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, id: ObjID, kind: NsNodeKind) -> Result<()> {
        tracing::debug!("{:?}: {} {:?}", name.as_ref(), id, kind);
        let (_node, container, remname) = self.namei(&name)?;
        container.insert(NsNode::new(kind, id, remname)?);
        Ok(())
    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Result<NsNode> {
        let (node, _) = self.namei_exist(name)?;
        Ok(node)
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> Result<std::vec::Vec<NsNode>> {
        tracing::debug!("enumerate: {:?}", name.as_ref());
        let (node, container) = self.namei_exist(name)?;
        if node.kind != NsNodeKind::Namespace {
            return Err(ErrorKind::NotADirectory);
        }
        tracing::debug!("opening namespace: {}", node.id);
        let ns = self.open_namespace(
            node.id,
            false,
            Some(ParentInfo::new(container, node.name())),
        )?;
        tracing::debug!("found namespace with {:?} items", ns.len());
        let items = ns.items();
        tracing::debug!("collected: {:?}", items);
        Ok(items)
    }

    pub fn enumerate_namespace_nsid(&self, id: ObjID) -> Result<std::vec::Vec<NsNode>> {
        let ns = self.open_namespace(id, false, None)?;
        let items = ns.items();
        Ok(items)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        let (node, container) = self.namei_exist(name)?;
        match node.kind {
            NsNodeKind::Namespace => {
                self.working_ns = Some(self.open_namespace(
                    node.id,
                    container.persist(),
                    Some(ParentInfo::new(container, node.name())),
                )?);
                Ok(())
            }
            NsNodeKind::Object => Err(ErrorKind::Other),
        }
    }

    pub fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        let (node, container) = self.namei_exist(&name)?;
        container
            .remove(node.name.as_str())
            .map(|_| ())
            .ok_or(ErrorKind::NotFound)
    }
}
