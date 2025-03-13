use std::{
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use arrayvec::ArrayString;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::{Object, ObjectBuilder},
    ptr::Ref,
};
use twizzler_abi::syscall::ObjectCreateError;
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
pub struct Namespace {
    obj: VecObject<NsNode, VecObjectAlloc>,
}

impl Namespace {
    pub fn new(volatile: bool) -> Result<Self> {
        let mut builder = ObjectBuilder::default();
        if !volatile {
            builder = builder.persist();
        }
        Ok(Self {
            obj: VecObject::new(builder).map_err(|_| ErrorKind::Other)?,
        })
    }

    pub fn open(id: ObjID) -> Result<Self> {
        Ok(Self {
            obj: VecObject::from(
                Object::map(id, MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST)
                    .map_err(|_| ErrorKind::Other)?,
            ),
        })
    }

    pub fn find(&self, name: &str) -> Option<&NsNode> {
        for entry in self.obj.iter() {
            if entry.name.as_str() == name {
                return Some(entry);
            }
        }
        None
    }

    pub fn insert(&mut self, node: NsNode) -> Option<NsNode> {
        self.obj.push(node).unwrap();
        None
    }

    pub fn remove(&mut self, name: &str) -> Option<NsNode> {
        for (idx, entry) in self.obj.iter().enumerate() {
            let entry = *entry;
            if entry.name.as_str() == name {
                self.obj.remove(idx).unwrap();
                return Some(entry);
            }
        }
        None
    }

    pub fn parent_id(&self) -> Option<ObjID> {
        self.find("..").map(|n| n.id)
    }

    pub fn id(&self) -> ObjID {
        self.obj.object().id()
    }

    pub fn iter(&self) -> NsIter<'_> {
        NsIter { ns: self, pos: 0 }
    }

    pub fn get_nth(&self, index: usize) -> Option<&NsNode> {
        self.obj.get(index)
    }
}

struct NsIter<'a> {
    ns: &'a Namespace,
    pos: usize,
}

impl<'a> Iterator for NsIter<'a> {
    type Item = &'a NsNode;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.ns.get_nth(self.pos)?;
        self.pos += 1;
        Some(item)
    }
}

pub struct NameStore {
    nameroot: Namespace,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let mut this = NameStore {
            nameroot: Namespace::new(true).unwrap(),
        };
        this.nameroot
            .insert(NsNode::ext(ArrayString::from("ext").unwrap()));
        this
    }

    // Loads in an existing object store from an Object ID
    pub fn new_with(id: ObjID) -> Result<NameStore> {
        let mut this = Self::new();
        this.nameroot
            .insert(NsNode::ns(ArrayString::from("data").unwrap(), id));
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
    working_ns: Option<Namespace>,
}

impl NameSession<'_> {
    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<'a, P: AsRef<Path>>(&self, name: P) -> Result<(Option<NsNode>, Namespace)> {
        let mut namespace = if name.as_ref().has_root() {
            self.store.nameroot.clone()
        } else {
            self.working_ns
                .as_ref()
                .unwrap_or(&self.store.nameroot)
                .clone()
        };
        let mut node: Option<NsNode> = None;
        // traverse store based on path's components
        for item in name.as_ref().components() {
            if let Some(node) = node.take() {
                if node.kind != NsNodeKind::Namespace {
                    return Err(ErrorKind::NotADirectory);
                }
                namespace = Namespace::open(node.id)?;
            }
            match item {
                Component::Prefix(_) => {
                    continue;
                }
                Component::RootDir => {
                    namespace = self.store.nameroot.clone();
                    continue;
                }
                Component::CurDir => continue,
                Component::ParentDir => {
                    let parent = namespace.parent_id().ok_or(ErrorKind::InvalidFilename)?;
                    namespace = Namespace::open(parent)
                        .ok()
                        .ok_or(ErrorKind::InvalidFilename)?;
                    continue;
                }
                Component::Normal(os_str) => {
                    node = namespace
                        .find(os_str.to_str().ok_or(ErrorKind::InvalidFilename)?)
                        .map(|r| *r);
                }
            }
        }

        Ok((node, namespace))
    }

    fn namei_exist<'a, P: AsRef<Path>>(&self, name: P) -> Result<(NsNode, Namespace)> {
        let (n, ns) = self.namei(name)?;
        Ok((n.ok_or(ErrorKind::NotFound)?, ns))
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, id: ObjID, kind: NsNodeKind) -> Result<()> {
        let (_node, mut container) = self.namei(&name)?;
        container.insert(NsNode::new(kind, id, name)?);
        Ok(())
    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Result<NsNode> {
        let (node, _) = self.namei_exist(name)?;
        Ok(node)
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> Result<std::vec::Vec<NsNode>> {
        let (node, _) = self.namei_exist(name)?;
        if node.kind != NsNodeKind::Namespace {
            return Err(ErrorKind::NotADirectory);
        }
        let ns = Namespace::open(node.id)?;
        Ok(ns.iter().map(|n| *n).collect())
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        let (node, _) = self.namei_exist(name)?;
        match node.kind {
            NsNodeKind::Namespace => {
                self.working_ns = Some(Namespace::open(node.id)?);
                Ok(())
            }
            NsNodeKind::Object => Err(ErrorKind::Other),
        }
    }

    pub fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        let (node, mut container) = self.namei_exist(&name)?;
        container
            .remove(node.name.as_str())
            .map(|_| ())
            .ok_or(ErrorKind::NotFound)
    }
}
