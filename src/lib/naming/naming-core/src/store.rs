use std::{
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use arrayvec::ArrayString;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::{Object, ObjectBuilder},
    ptr::Ref,
};
use twizzler_rt_abi::object::{MapError, MapFlags, ObjID};

use crate::{Result, MAX_KEY_SIZE};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum NsNodeKind {
    Namespace,
    Object,
}
unsafe impl Invariant for NsNodeKind {}

const NSID_EXTERNAL: ObjID = ObjID::new(1);
const NSID_ROOT: ObjID = ObjID::new(0);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct NsNode {
    pub kind: NsNodeKind,
    pub id: ObjID,
    pub name: ArrayString<MAX_KEY_SIZE>,
}
unsafe impl Invariant for NsNode {}

impl NsNode {
    pub fn new<P: AsRef<Path>>(kind: NsNodeKind, id: ObjID, name: P) -> Result<Self> {
        let name = ArrayString::from(name.as_ref().to_str().ok_or(ErrorKind::InvalidFilename)?)
            .map_err(|_| ErrorKind::InvalidFilename)?;
        Ok(Self { kind, id, name })
    }

    fn ext(name: ArrayString<MAX_KEY_SIZE>) -> Self {
        Self::ns(name, NSID_EXTERNAL)
    }

    fn ns(name: ArrayString<MAX_KEY_SIZE>, id: ObjID) -> Self {
        Self {
            kind: NsNodeKind::Namespace,
            id,
            name,
        }
    }
}

#[derive(Clone)]
pub struct NamespaceObject {
    persist: bool,
    obj: Arc<Mutex<Option<VecObject<NsNode, VecObjectAlloc>>>>,
}

#[derive(Clone)]
struct ExtNamespace {
    name: Option<String>,
    prefix: PathBuf,
    parent: Option<ObjID>,
}
impl Namespace for ExtNamespace {
    fn new(_persist: bool, parent: Option<ObjID>) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self {
            name: None,
            parent,
            prefix: "/".to_owned().into(),
        })
    }

    fn open(_id: ObjID, _persist: bool) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(Self {
            name: None,
            parent: None,
            prefix: "/".to_owned().into(),
        })
    }

    fn find(&self, name: &str) -> Option<NsNode> {
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            let mut path = self.prefix.clone();
            path.push(name);
            let (id, is_ns) = h.stat_external(&path).ok()?;
            let kind = if is_ns {
                NsNodeKind::Namespace
            } else {
                NsNodeKind::Object
            };
            NsNode::new(kind, id, name).ok()
        } else {
            None
        }
    }

    fn insert(&self, _node: NsNode) -> Option<NsNode> {
        None
    }

    fn remove(&self, _name: &str) -> Option<NsNode> {
        None
    }

    fn id(&self) -> ObjID {
        NSID_EXTERNAL
    }

    fn persist(&self) -> bool {
        false
    }

    fn items(&self) -> Vec<NsNode> {
        if let Some(mut h) = pager_dynamic::PagerHandle::new() {
            if let Ok(items) = h.enumerate_external(&self.prefix) {
                return items
                    .iter()
                    .map(|i| NsNode::new(NsNodeKind::Object, 0.into(), i).unwrap())
                    .collect();
            }
        }
        vec![]
    }
}

trait Namespace {
    fn new(persist: bool, parent: Option<ObjID>) -> Result<Self>
    where
        Self: Sized;

    fn open(id: ObjID, persist: bool) -> Result<Self>
    where
        Self: Sized;

    fn find(&self, name: &str) -> Option<NsNode>;

    fn insert(&self, node: NsNode) -> Option<NsNode>;

    fn remove(&self, name: &str) -> Option<NsNode>;

    fn parent_id(&self) -> Option<ObjID> {
        self.find("..").map(|n| n.id)
    }

    fn id(&self) -> ObjID;

    fn items(&self) -> Vec<NsNode>;

    fn len(&self) -> usize {
        self.items().len()
    }

    fn persist(&self) -> bool;
}

impl NamespaceObject {
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
    fn new(persist: bool, parent: Option<ObjID>) -> Result<Self> {
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

pub struct NameStore {
    nameroot: Arc<dyn Namespace>,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let this = NameStore {
            nameroot: Arc::new(NamespaceObject::new(false, None).unwrap()),
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
        this.change_namespace(namespace);
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
    fn open_namespace(&self, id: ObjID, persist: bool) -> Result<Arc<dyn Namespace>> {
        Ok(if id == NSID_EXTERNAL {
            Arc::new(ExtNamespace::open(id, persist)?)
        } else {
            Arc::new(NamespaceObject::open(id, persist)?)
        })
    }

    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<P: AsRef<Path>>(
        &self,
        name: P,
    ) -> Result<(Option<NsNode>, Arc<dyn Namespace>, PathBuf)> {
        let mut namespace = if name.as_ref().has_root() {
            &self.store.nameroot
        } else {
            self.working_ns.as_ref().unwrap_or(&self.store.nameroot)
        }
        .clone();
        let mut node: Option<NsNode> = None;
        tracing::debug!("namei: {:?}", name.as_ref());
        let mut remname = name.as_ref().to_owned();
        // traverse store based on path's components
        for item in name.as_ref().components() {
            if let Some(node) = node.take() {
                if node.kind != NsNodeKind::Namespace {
                    return Err(ErrorKind::NotADirectory);
                }
                tracing::debug!("traversing to {} => {}", node.name, node.id);
                namespace = self.open_namespace(node.id, namespace.persist())?;
            }
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
                    let parent = namespace.parent_id().ok_or(ErrorKind::InvalidFilename)?;
                    namespace = self
                        .open_namespace(parent, namespace.persist())
                        .ok()
                        .ok_or(ErrorKind::InvalidFilename)?;
                    remname = PathBuf::from("..");
                    continue;
                }
                Component::Normal(os_str) => {
                    tracing::debug!("lookup component {:?}", os_str);
                    node = namespace.find(os_str.to_str().ok_or(ErrorKind::InvalidFilename)?);
                    tracing::debug!("again from the top");
                    remname = PathBuf::from(os_str);
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
        let ns = NamespaceObject::new(persist, Some(container.id()))?;
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
        let (node, _) = self.namei_exist(name)?;
        if node.kind != NsNodeKind::Namespace {
            return Err(ErrorKind::NotADirectory);
        }
        tracing::debug!("opening namespace: {}", node.id);
        let ns = self.open_namespace(node.id, false)?;
        tracing::debug!("found namespace with {:?} items", ns.len());
        let items = ns.items();
        tracing::debug!("collected: {:?}", items);
        Ok(items)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        let (node, container) = self.namei_exist(name)?;
        match node.kind {
            NsNodeKind::Namespace => {
                self.working_ns = Some(self.open_namespace(node.id, container.persist())?);
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
