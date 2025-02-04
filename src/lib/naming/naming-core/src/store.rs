use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use arrayvec::ArrayString;
use twizzler::{
    collections::vec::{VecObject, VecObjectAlloc},
    marker::Invariant,
    object::ObjectBuilder,
    ptr::Ref,
};

use crate::{error::ErrorKind, Result, MAX_KEY_SIZE};

// Currently the way namespaces exist is each entry has a parent,
// And to determine the children of an entry, you linearly search
// for each entry's parent

// The short answer this is it will be gone once indirection exists
// But I wanted to create the interface first so I can replace it
// later

#[derive(Default, Debug, Eq, PartialEq, Clone, Copy, PartialOrd, Ord)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum EntryType {
    Namespace,
    Object(u128),
    #[default]
    Name,
}

#[repr(C)]
#[derive(Debug, Default, Eq, PartialEq, Clone, Copy)]
pub struct Entry {
    pub name: ArrayString<MAX_KEY_SIZE>,
    pub entry_type: EntryType,
}

impl Entry {
    pub fn try_new<P: AsRef<Path>>(name: P, entry_type: EntryType) -> Result<Entry> {
        Ok(Entry {
            name: ArrayString::from(name.as_ref().to_str().ok_or(ErrorKind::InvalidName)?)
                .map_err(|_| ErrorKind::InvalidName)?,
            entry_type,
        })
    }
}

#[repr(C)]
#[derive(Debug, Eq, PartialEq)]
struct Node {
    parent: usize,
    curr: usize,
    entry: Entry,
}

unsafe impl Invariant for Node {}

// Ideally when transactions are finished the mutex is unnecessary
// Though I don't know how to write this without the mutex :think:
pub struct NameStore {
    name_universe: Mutex<VecObject<Node, VecObjectAlloc>>,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

// This is atrociously inefficient, but once indirection starts
// existing I can finally make this a tree instead of a flat vec
impl NameStore {
    pub fn new() -> NameStore {
        let mut store = VecObject::new(ObjectBuilder::default()).unwrap();
        store
            .push(Node {
                parent: 0,
                curr: 0,
                entry: Entry::try_new("/", EntryType::Namespace).unwrap(),
            })
            .unwrap();
        NameStore {
            name_universe: Mutex::new(store),
        }
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

// Hopefully this session will do transactions! That will solve all my problems
// and data races...
pub struct NameSession<'a> {
    store: &'a NameStore,
    working_ns: PathBuf,
}

impl NameSession<'_> {
    // This function will return a reference to an entry described by name: P relative to working_ns
    // If the name is absolute then it will start at root instead of the working_ns
    fn namei<'a, P: AsRef<Path>>(
        &self,
        store: &'a MutexGuard<'a, VecObject<Node, VecObjectAlloc>>,
        name: P,
    ) -> Result<Ref<'a, Node>> {
        // interpret path based on working directory
        let path = match name.as_ref().has_root() {
            true => PathBuf::from(name.as_ref()),
            false => {
                let mut path = self.working_ns.clone();
                path.extend(name.as_ref());
                path
            }
        };

        let mut index = 0;
        // traverse store based on path's components
        for item in path.components() {
            let mut found = false;
            match item {
                Component::Prefix(_) => {
                    continue;
                }
                Component::RootDir => {
                    index = 0;
                    continue;
                }
                Component::CurDir => continue,
                Component::ParentDir => {
                    index = store.get(index).unwrap().parent;
                    continue;
                }
                Component::Normal(os_str) => {
                    for i in 0..store.len() {
                        let node = store.get(i).unwrap();
                        if node.entry.name.as_str()
                            == os_str.to_str().ok_or(ErrorKind::InvalidName)?
                            && node.parent == index
                        {
                            index = i;
                            found = true;
                            break;
                        }
                    }
                }
            }

            if !found {
                return Result::Err(ErrorKind::NotFound);
            }
        }

        Ok(store.get(index).unwrap())
    }

    // Traverses the path and construct the canonical path given name relative to absolute path
    fn construct_canonical<'a, P: AsRef<Path>>(
        &self,
        store: &'a MutexGuard<'a, VecObject<Node, VecObjectAlloc>>,
        name: P,
    ) -> Result<(PathBuf, EntryType)> {
        let mut vec = VecDeque::<String>::new();

        let mut node = self.namei(&store, &name)?;

        let mut current = node.curr;
        while current != 0 {
            node = store.get(current).unwrap();
            vec.push_front(node.entry.name.to_string());
            current = node.parent;
        }

        vec.push_front("/".to_owned());

        Ok((PathBuf::from_iter(vec), node.entry.entry_type))
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, val: EntryType) -> Result<()> {
        let mut store = self
            .store
            .name_universe
            .lock()
            .map_err(|_| ErrorKind::Other)?;
        let entry = {
            let current_entry = self.namei(&store, &name);
            let _ = match current_entry {
                Ok(node) => {
                    unsafe {
                        let mut mut_node = node.mutable();
                        if mut_node.entry.entry_type != EntryType::Namespace {
                            mut_node.entry.entry_type = val;
                        }
                    }

                    return Ok(());
                }
                Err(x) => match x {
                    ErrorKind::NotFound => Entry::try_new(&name, val),
                    _ => return Err(x),
                },
            };

            let entry = match name.as_ref().parent() {
                Some(parent) => self.namei(&store, parent)?,
                None => {
                    return Err(ErrorKind::InvalidName);
                } // ends in root or prefix
            };

            let child = name.as_ref().file_name().ok_or(ErrorKind::InvalidName)?;

            Node {
                parent: entry.curr,
                curr: store.len(),
                entry: Entry::try_new(child, val)?,
            }
        };

        store.push(entry).unwrap();

        Ok(())
    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Result<Entry> {
        let store = self
            .store
            .name_universe
            .lock()
            .map_err(|_| ErrorKind::Other)?;
        let node = self.namei(&store, name)?;

        let entry = (*node).entry;
        Ok(entry)
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> Result<std::vec::Vec<Entry>> {
        let store = self
            .store
            .name_universe
            .lock()
            .map_err(|_| ErrorKind::Other)?;

        let mut vec = std::vec::Vec::new();

        let node = self.namei(&store, name)?;

        if node.entry.entry_type != EntryType::Namespace {
            return Result::Err(ErrorKind::NotNamespace);
        }

        for i in 1..store.len() {
            let search = store.get(i).unwrap();
            if search.parent == node.curr {
                vec.push((*search).entry);
            }
        }

        Ok(vec)
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) -> Result<()> {
        let store = self
            .store
            .name_universe
            .lock()
            .map_err(|_| ErrorKind::Other)?;
        let (canonical_name, entry) = self.construct_canonical(&store, name)?;
        match entry {
            EntryType::Namespace => {
                self.working_ns = PathBuf::from(canonical_name);
                Ok(())
            }
            _ => Result::Err(ErrorKind::NotNamespace),
        }
    }

    // It's good that this doesn't exist yet because it would be really bad if it did
    pub fn remove<P: AsRef<Path>>(&self, _name: P) {
        todo!()
    }
}
