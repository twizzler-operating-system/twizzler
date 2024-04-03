use std::{collections::HashMap, sync::Mutex};

use dynlink::engines::Engine;
use twizzler_runtime_api::ObjID;

use self::runcomp::RunComp;

mod object;
mod runcomp;
mod thread;

struct CompMan {
    inner: Mutex<CompManInner>,
    dynlink: Mutex<Option<dynlink::context::Context<Engine>>>,
}

lazy_static::lazy_static! {
static ref COMPMAN: CompMan = CompMan::new();
}

impl CompMan {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CompManInner::default()),
            dynlink: Mutex::new(None),
        }
    }
}

#[derive(Default)]
struct CompManInner {
    name_map: HashMap<String, ObjID>,
    instance_map: HashMap<ObjID, RunComp>,
}

impl CompManInner {
    pub fn insert(&mut self, rc: RunComp) {
        self.name_map.insert(rc.name().to_string(), rc.instance);
        self.instance_map.insert(rc.instance, rc);
    }

    pub fn lookup(&mut self, instance: ObjID) -> Option<&RunComp> {
        self.instance_map.get(&instance)
    }

    pub fn lookup_name(&mut self, name: &str) -> Option<&RunComp> {
        self.lookup(*self.name_map.get(name)?)
    }

    pub fn lookup_instance(&mut self, name: &str) -> Option<ObjID> {
        self.name_map.get(name).cloned()
    }

    pub fn remove(&mut self, instance: ObjID) -> Option<RunComp> {
        let Some(rc) = self.instance_map.remove(&instance) else {
            return None;
        };
        self.name_map.remove(rc.name());
        Some(rc)
    }
}
