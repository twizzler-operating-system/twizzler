use std::collections::{HashMap};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct FsNamer {
    mappings: HashMap<String, u64>,
    
}

impl FsNamer {
    pub fn new() -> Self {
        Self {
            mappings: HashMap::new(),
        }
    }
}

impl FsNamer {
    pub fn insert(&mut self, name: String, id: u64) -> Option<u64> {
        self.mappings.insert(name, id)
    }

    pub fn get(&self, name: &String) -> Option<&u64> {
        self.mappings.get(name)
    }

    pub fn remove(&mut self, name: &String) -> Option<u64> {
        self.mappings.remove(name)
    }
}
