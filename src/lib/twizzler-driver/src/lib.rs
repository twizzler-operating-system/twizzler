#![feature(vec_into_raw_parts)]
#![feature(auto_traits)]
#![feature(negative_impls)]
use device::DeviceChildrenIterator;
use twizzler_abi::kso::{KactionCmd, KactionFlags, KactionGenericCmd};
use twizzler_object::ObjID;

mod arch;
pub mod bus;
mod controller;
pub mod device;
pub mod dma;
pub mod request;

pub use controller::DeviceController;

/// A handle for the root of the bus tree.
pub struct BusTreeRoot {
    root_id: ObjID,
}

impl BusTreeRoot {
    /// Get the children of the bus tree.
    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.root_id,
            pos: 0,
        }
    }
}

/// Get a handle to the root of the bus tree.
pub fn get_bustree_root() -> BusTreeRoot {
    let cmd = KactionCmd::Generic(KactionGenericCmd::GetKsoRoot);
    let id = twizzler_abi::syscall::sys_kaction(cmd, None, 0, 0, KactionFlags::empty())
        .expect("failed to get device root")
        .unwrap_objid();
    BusTreeRoot { root_id: id }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
