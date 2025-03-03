use std::{
    collections::{HashSet, VecDeque},
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
use twizzler_rt_abi::object::{MapFlags, ObjID};

use crate::{error::ErrorKind, Result, MAX_KEY_SIZE};

// Currently the way namespaces exist is each entry has a parent,
// And to determine the children of an entry, you linearly search
// for each entry's parent

// The short answer this is it will be gone once indirection exists
// But I wanted to create the interface first so I can replace it
// later

#[derive(Default, Debug, Eq, PartialEq, Clone, Copy, PartialOrd, Ord)]
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

#[allow(dead_code)]
impl Node {
    fn is_namespace(&self) -> bool {
        self.entry.entry_type == EntryType::Namespace
    }

    fn is_object(&self) -> bool {
        self.entry.entry_type == EntryType::Namespace
    }

    fn id(&self) -> Option<u128> {
        match self.entry.entry_type {
            EntryType::Namespace => None,
            EntryType::Object(x) => Some(x),
            EntryType::Name => None,
        }
    }

    fn name(&self) -> ArrayString<MAX_KEY_SIZE> {
        self.entry.name.clone()
    }
}

unsafe impl Invariant for Node {}

// Ideally when transactions are finished the mutex is unnecessary
// Though I don't know how to write this without the mutex :think:
pub struct NameStore {
    name_universe: Mutex<VecObject<Node, VecObjectAlloc>>,
    backing_id: ObjID,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

// This is atrociously inefficient, but once indirection starts
// existing I can finally make this a tree instead of a flat vec
impl NameStore {
    pub fn new() -> NameStore {
        let mut store = VecObject::new(ObjectBuilder::default().persist()).unwrap();
        store
            .push(Node {
                parent: 0,
                curr: 0,
                entry: Entry::try_new("/", EntryType::Namespace).unwrap(),
            })
            .unwrap();
        let id = store.object().id();
        NameStore {
            name_universe: Mutex::new(store),
            backing_id: id,
        }
    }

    // Loads in an existing object store from an Object ID
    pub fn new_in(id: ObjID) -> Result<NameStore> {
        let mut store = VecObject::from(
            Object::map(id, MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST)
                .map_err(|_| ErrorKind::NotFound)?,
        );

        // todo make "/" not an entry
        if store.get(0).is_none() {
            store
                .push(Node {
                    parent: 0,
                    curr: 0,
                    entry: Entry::try_new("/", EntryType::Namespace).unwrap(),
                })
                .unwrap();
        }
        Ok(NameStore {
            name_universe: Mutex::new(store),
            backing_id: id,
        })
    }

    pub fn id(&self) -> ObjID {
        self.backing_id
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

        Ok(store.get_ref(index).unwrap())
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
            node = store.get_ref(current).unwrap();
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

    pub fn remove<P: AsRef<Path>>(&self, name: P, recursive: bool) -> Result<()> {
        let mut store = self
            .store
            .name_universe
            .lock()
            .map_err(|_| ErrorKind::Other)?;

        let entry = self.namei(&store, &name)?;
        let index = entry.curr;
        if !recursive && entry.entry.entry_type == EntryType::Namespace {
            return Err(ErrorKind::NotFile);
        }
        if entry.curr == 0 {
            return Err(ErrorKind::InvalidName);
        }

        drop(entry);

        // Copies a node to another index. If it's a directory
        // it will fix all the child nodes if they exist
        unsafe fn swap_node(
            store: &MutexGuard<VecObject<Node, VecObjectAlloc>>,
            old: usize,
            new: usize,
        ) {
            let mut old_node = store.get_ref(old).unwrap().mutable();
            let mut new_node = store.get_ref(new).unwrap().mutable();
            std::ptr::swap(old_node.raw(), new_node.raw());
            std::mem::swap(&mut old_node.curr, &mut new_node.curr);

            for i in 0..store.len() {
                let mut node = unsafe { store.get_ref(i).unwrap().mutable() };
                // If the node's parent is pointing to where the swapped node is, fix it
                if old_node.is_namespace() && node.parent == new_node.curr {
                    node.parent = old_node.curr;
                }
                if new_node.is_namespace() && node.parent == old_node.curr {
                    node.parent = new_node.curr;
                }
            }
        }

        fn recurse_helper(
            store: &MutexGuard<VecObject<Node, VecObjectAlloc>>,
            set: &mut HashSet<usize>,
            index: usize,
        ) {
            let node: Ref<'_, Node> = store.get_ref(index).unwrap();
            set.insert(index);
            if !node.is_namespace() {
                return;
            }
            for i in 1..store.len() {
                let candidate = store.get(i).unwrap();
                if candidate.parent != node.curr {
                    continue;
                }
                recurse_helper(store, set, candidate.curr);
            }
        }

        if recursive {
            let mut candidates = HashSet::new();
            recurse_helper(&store, &mut candidates, index);
            let candidates_num = candidates.len();
            // Swap valid nodes to the left with all invalid nodes to the right
            // Then trim the vector of invalid nodes
            let mut left: usize = 1;
            let mut right: usize = store.len() - 1;
            while left < right {
                let left_node: Ref<'_, Node> = store.get_ref(left).unwrap();
                let right_node: Ref<'_, Node> = store.get_ref(right).unwrap();

                // I want the right node to contain a valid node that is able to be swapped
                // If the left node contains an invalid node...
                match (
                    candidates.contains(&left_node.curr),
                    candidates.contains(&right_node.curr),
                ) {
                    (true, true) => {
                        right -= 1; // right fish for valid
                    }
                    (true, false) => {
                        candidates.remove(&left);
                        unsafe { swap_node(&store, left, right) }; // swap left and right
                        right -= 1;
                    }
                    (false, true) => {
                        right -= 1;
                    }
                    (false, false) => {
                        left += 1; // left fish for invalid
                    }
                }
            }

            // pop off all the candidates
            for _ in 0..candidates_num {
                let end = store.len();
                store.remove(end - 1).unwrap();
            }
        } else {
            unsafe { swap_node(&store, index, store.len() - 1) };
            let end = store.len();
            store.remove(end - 1).unwrap();
        }

        Ok(())
    }
}
