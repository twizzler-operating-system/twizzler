use object_store::PagedObjectStore;
use twizzler::object::ObjID;

pub type DynStore = dyn PagedObjectStore + Send + Sync;

/// Serves every object whose ID satisfies `id & mask == value`.
pub struct Route {
    pub mask: u128,
    pub value: u128,
    pub store: Box<DynStore>,
}

impl Route {
    pub fn claims(&self, id: ObjID) -> bool {
        id.raw() & self.mask == self.value
    }
}
