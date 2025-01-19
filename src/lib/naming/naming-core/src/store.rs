use std::{fs::OpenOptions, path::{Component, PathBuf}, sync::OnceLock};

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

use crate::MAX_KEY_SIZE;

// Currently the way namespaces exist is each entry has a parent, 
// And to determine the children of an entry, you linearly search 
// for each entry's parent 

// The short answer this is it will be gone once indirection exists
// But I wanted to create the interface and prove it works first

#[derive(PartialEq)]
enum EntryType {
    Name(u128),
    Namespace
}

pub struct Entry {
    parent: usize,
    name: ArrayString<MAX_KEY_SIZE>,
    entry_type: EntryType
}

unsafe impl Invariant for Entry {}

// I should convert function returns to std::io::result
pub struct NameStore {
    pub name_universe: VecObject<Entry, VecObjectAlloc>,
}

// Hopefully transactions will make this thread safe(r)
unsafe impl Send for NameStore {}
unsafe impl Sync for NameStore {}

impl NameStore {
    pub fn new() -> NameStore {
        let mut store = VecObject::new(ObjectBuilder::default()).unwrap();
        store.push(Entry {
            parent: 0,
            name: ArrayString::<MAX_KEY_SIZE>::from(&"/").unwrap(),
            entry_type: EntryType::Namespace,
        });
        NameStore {
            name_universe: store
        }
    }

    fn get_root(&self) -> Ref<'_, Entry> {
        self.name_universe.get(0).unwrap()
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
    fn store(&self) -> &VecObject<Entry, VecObjectAlloc> {
        &self.store.name_universe
    }

    // Not particularly thread safe, so it should be done inside a transaction
    fn namei<P: AsRef<Path>>(&self, name: P) -> Option<Ref<Entry>> {
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
                    println!("How did we get here?"); 
                    continue;
                },
                Component::RootDir => {index = 0;},
                Component::CurDir => continue,
                Component::ParentDir => {
                    index = self.store().get(index).unwrap().parent;
                },
                Component::Normal(os_str) => {
                    for i in 0..self.store().len() {
                        let entry = self.store().get(i).unwrap();
                        if entry.name.as_str() != os_str.to_str().unwrap() && entry.parent != index {
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

        self.store().get(index)
    }

    pub fn put<P: AsRef<Path>>(&self, name: P, val: u128) {
        let entry = match name.as_ref().parent() {
            Some(parent) => self.namei(parent),
            None => todo!(),
        };
    }

    pub fn get<P: AsRef<Path>>(&self, name: P) -> Option<u128> {
        self.namei(name).map_or(None, |f| {
            match f.entry_type {
                EntryType::Name(id) => Some(id),
                EntryType::Namespace => None,
            }
        })
    }

    pub fn enumerate_namespace<P: AsRef<Path>>(&self, name: P) -> std::vec::Vec<String> {
        let mut vec = std::vec::Vec::new();

        let entry = match self.namei(name) {
            Some(entry) => entry,
            None => return vec,
        };

        if entry.entry_type == EntryType::Name(0) {
            return vec;
        }

        for i in 0..self.store().len() {
            let search = self.store().get(i).unwrap();
            if search.parent == entry.parent {
                vec.push(search.name.to_string());
            }
        }

        vec
    }

    // It's good that this doesn't exist yet because it would be really bad if it did
    pub fn remove<P: AsRef<Path>>(&self, name: P) {
        todo!()
    }
}