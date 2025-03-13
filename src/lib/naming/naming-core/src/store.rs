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

const NSID_EXTERNAL: u128 = 1;
const NSID_ROOT: u128 = 0;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct NsNode {
    pub kind: NsNodeKind,
    pub id: u128,
    pub name: ArrayString<MAX_KEY_SIZE>,
}
unsafe impl Invariant for NsNode {}

impl NsNode {
    fn ext(name: ArrayString<MAX_KEY_SIZE>) -> Self {
        Self::ns(name, NSID_EXTERNAL)
    }

    fn ns(name: ArrayString<MAX_KEY_SIZE>, id: u128) -> Self {
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
        todo!()
    }

    pub fn open(id: ObjID) -> Result<Self> {
        todo!()
    }

    pub fn find(&self, name: &str) -> Option<Ref<'_, NsNode>> {
        todo!()
    }

    pub fn insert(&self, node: NsNode) -> Option<NsNode> {
        todo!()
    }

    pub fn remove(&self, name: &str) -> Option<NsNode> {
        todo!()
    }

    pub fn parent_id(&self) -> Option<ObjID> {
        todo!()
    }

    pub fn id(&self) -> ObjID {
        todo!()
    }

    pub fn iter(&self) -> NsIter<'_> {
        todo!()
    }

    pub fn get_nth(&self, index: usize) -> Option<Ref<'_, NsNode>> {
        todo!()
    }
}

struct NsIter<'a> {
    ns: &'a Namespace,
    pos: usize,
}

impl<'a> Iterator for NsIter<'a> {
    type Item = Ref<'a, NsNode>;

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
        let this = NameStore {
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
            .insert(NsNode::ns(ArrayString::from("data").unwrap(), id.raw()));
        Ok(this)
    }

    pub fn id(&self) -> ObjID {
        self.nameroot.id()
    }

    // session is created from root
    pub fn new_session(&self, namespace: &Path) -> NameSession<'_> {
        let mut path = PathBuf::from("/");
        path.extend(namespace);
        NameSession {
            store: self,
            working_ns: path,
        }
    }

    pub fn root_session(&self) -> NameSession<'_> {
        NameSession {
            store: self,
            working_ns: PathBuf::from("/"),
        }
    }
}

pub struct NameSession<'a> {
    store: &'a NameStore,
    working_ns: PathBuf,
}

impl NameSession<'_> {
    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<'a, P: AsRef<Path>>(&self, name: P) -> Result<Ref<'a, NsNode>> {
        // interpret path based on working directory
        let path = match name.as_ref().has_root() {
            true => PathBuf::from(name.as_ref()),
            false => {
                let mut path = self.working_ns.clone();
                path.extend(name.as_ref());
                path
            }
        };

        let mut namespace = self.store.nameroot.clone();
        let mut node = None;
        // traverse store based on path's components
        for item in path.components() {
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
                    let thisnode = namespace
                        .find(os_str.to_str().ok_or(ErrorKind::InvalidFilename)?)
                        .ok_or(ErrorKind::InvalidFilename)?;

                    node = Some(thisnode.owned());
                }
            }
        }

        node.ok_or(ErrorKind::InvalidFilename)
    }

    // Traverses the path and construct the canonical path given name relative to absolute path
    fn construct_canonical<'a, P: AsRef<Path>>(&self, name: P) -> Result<(PathBuf, NsNodeKind)> {
        todo!()
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, id: ObjID, kind: NsNodeKind) -> Result<()> {
        todo!()
    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Result<NsNode> {
        todo!()
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> Result<std::vec::Vec<NsNode>> {
        todo!()
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        let (canonical_name, entry) = self.construct_canonical(name)?;
        match entry {
            NsNodeKind::Namespace => {
                self.working_ns = PathBuf::from(canonical_name);
                Ok(())
            }
            _ => Result::Err(ErrorKind::NotADirectory),
        }
    }

    pub fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        todo!()
    }
}
