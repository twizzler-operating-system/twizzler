#![feature(option_result_unwrap_unchecked)]
#![feature(generic_associated_types)]

use device::children::DeviceChildrenIterator;
use twizzler_abi::kso::{KactionCmd, KactionFlags, KactionGenericCmd};
use twizzler_object::ObjID;

pub mod bus;
pub mod device;

pub struct BusTreeRoot {
    root_id: ObjID,
}

impl BusTreeRoot {
    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.root_id,
            pos: 0,
        }
    }
}

pub fn get_bustree_root() -> BusTreeRoot {
    let cmd = KactionCmd::Generic(KactionGenericCmd::GetKsoRoot);
    let id = twizzler_abi::syscall::sys_kaction(cmd, None, 0, KactionFlags::empty())
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
