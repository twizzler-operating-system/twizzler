use std::{fs::OpenOptions, path::{Component, PathBuf}, sync::{MutexGuard, OnceLock}};

use arrayvec::ArrayString;
use monitor_api::CompartmentHandle;
use secgate::{
    util::{Descriptor, Handle, SimpleBuffer},
    DynamicSecGate, SecGateReturn,
};
use twizzler_rt_abi::object::{MapFlags, ObjID};
use twizzler::{
    alloc::invbox::InvBox, collections::vec::{Vec, VecObject, VecObjectAlloc}, marker::Invariant, object::{Object, ObjectBuilder, TypedObject}, ptr::{GlobalPtr, InvPtr}
};
use twizzler::ptr::Ref;
use std::path::Path;
use std::sync::Mutex;

use crate::{handle::Schema, MAX_KEY_SIZE};

// Currently the way namespaces exist is each entry has a parent, 
// And to determine the children of an entry, you linearly search 
// for each entry's parent 

// The short answer this is it will be gone once indirection exists
// But I wanted to create the interface first so I can replace it 
// later

#[derive(Debug, PartialEq)]
pub enum EntryType {
    Name(u128),
    Namespace
}

impl Default for EntryType {
    fn default() -> Self {
        EntryType::Name(0)
    }
}

#[derive(Debug, Default)]
pub struct Entry {
    parent: usize,
    curr: usize,
    name: ArrayString<MAX_KEY_SIZE>,
    entry_type: EntryType
}

unsafe impl Invariant for Entry {}

// Ideally when transactions are finished the mutex is unnecessary
// Though I don't know how to write this without the mutex :think:
pub struct NameStore {
    pub name_universe: Mutex<VecObject<Entry, VecObjectAlloc>>,
}

unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let mut store = VecObject::new(ObjectBuilder::default()).unwrap();
        store.push(Entry {
            parent: 0,
            curr: 0,
            name: ArrayString::<MAX_KEY_SIZE>::from(&"/").unwrap(),
            entry_type: EntryType::Namespace,
        });
        NameStore {
            name_universe: Mutex::new(store)
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
            working_ns: PathBuf::from("/")
        }
    }
}

// Hopefully this session will do transactions! That will solve all my problems
// and data races...
pub struct NameSession<'a> {
    store: &'a NameStore,
    working_ns: PathBuf
}

impl NameSession<'_> {
    fn namei<'a, P: AsRef<Path>>(&self, store: &'a MutexGuard<'a, VecObject<Entry, VecObjectAlloc>>, name: P) -> Option<Ref<'a, Entry>> {
        // interpret path based on working directory
        let path = match name.as_ref().has_root() {
            true => {
                PathBuf::from(name.as_ref())
            },
            false => {
                let mut path = self.working_ns.clone();
                path.extend(name.as_ref());
                path
            }
        };

        let path_child = path.file_name();
        let mut index = 0;
        // traverse store based on path's components
        for item in path.components() {
            let mut found = false;
            match item {
                Component::Prefix(prefix_component) => {
                    continue;
                },
                Component::RootDir => {index = 0; continue;},
                Component::CurDir => continue,
                Component::ParentDir => {
                    index = store.get(index).unwrap().parent;
                },
                Component::Normal(os_str) => {
                    for i in 0..store.len() {
                        let entry = store.get(i).unwrap();
                        if entry.name.as_str() != os_str.to_str().unwrap() || entry.parent != index {
                            continue;
                        }

                        index = i;
                        found = true;
                    }
                },
            }

            if !found {
                return None;
            }
        }

        store.get(index)
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, val: EntryType) {
        let mut store = self.store.name_universe.lock().unwrap();
        let entry = {
            let current_entry = self.namei(&store, &name);
            match current_entry {
                Some(entry) => {
                    unsafe { 
                        let mut entry: *mut Entry = entry.raw().cast_mut();
                        if (*entry).entry_type != EntryType::Namespace {
                            (*entry).entry_type = val; 
                        }
                    }

                    return; 
                },
                None => Entry::default()
            };
    
            let path_ref = match name.as_ref().parent() {
                Some(parent) => self.namei(&store, parent),
                None => {return;}, // ends in root or prefix
            };
            let entry = path_ref.unwrap();
    
            let child = name.as_ref().file_name();
            if child == None {
                return;
            }
    
            Entry {
                parent: entry.curr,
                curr: store.len(),
                name: ArrayString::from(&child.unwrap().to_str().unwrap()).unwrap(),
                entry_type: val, 
            }
        };

        store.push(entry);

    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Option<u128> {
        let store = self.store.name_universe.lock().unwrap();
        let entry = self.namei(&store, name);

        entry.map_or(None, |f| {
            match f.entry_type {
                EntryType::Name(id) => Some(id),
                EntryType::Namespace => None,
            }
        })
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> std::vec::Vec<Schema> {
        let store = self.store.name_universe.lock().unwrap();

        let mut vec = std::vec::Vec::new();

        let entry = match self.namei(&store, name) {
            Some(entry) => entry,
            None => return vec,
        };

        if entry.entry_type != EntryType::Namespace {
            return vec;
        }

        for i in 0..store.len() {
            let search = store.get(i).unwrap();
            println!("searching {:?}, curr {:?}", search, entry);
            if search.parent == entry.curr {
                vec.push(Schema {
                    key: ArrayString::from(&search.name.to_string()).unwrap(),
                    val: match search.entry_type {
                        EntryType::Name(x) => x,
                        EntryType::Namespace => 0,
                    },
                });
            }
        }

        vec
    }

    pub fn change_namespace<P: AsRef<Path>>(&mut self, name: P) {
        let store = self.store.name_universe.lock().unwrap();

        let entry = self.namei(&store, &name);
        
        let entry = if entry.is_none() {
            return;
        } else {
            entry.unwrap()
        };

        match entry.entry_type {
            EntryType::Name(_) => {return;},
            EntryType::Namespace => {
                self.working_ns = PathBuf::from(name.as_ref());
            },
        }
    }
    
    // It's good that this doesn't exist yet because it would be really bad if it did
    pub fn remove<P: AsRef<Path>>(&self, name: P) {
        todo!()
    }
}