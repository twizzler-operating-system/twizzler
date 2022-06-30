use twizzler_abi::kso::{KactionCmd, KactionFlags, KactionGenericCmd};
use twizzler_object::ObjID;

use super::Device;

pub struct DeviceChildrenIterator {
    pub(crate) id: ObjID,
    pub(crate) pos: u16,
}

impl Iterator for DeviceChildrenIterator {
    type Item = Device;
    fn next(&mut self) -> Option<Self::Item> {
        let cmd = KactionCmd::Generic(KactionGenericCmd::GetChild(self.pos));
        let result =
            twizzler_abi::syscall::sys_kaction(cmd, Some(self.id), 0, KactionFlags::empty())
                .ok()?;
        self.pos += 1;
        result.objid().map(|id| Device::new(id).ok()).flatten()
    }
}

impl Device {
    pub fn children(&self) -> DeviceChildrenIterator {
        DeviceChildrenIterator {
            id: self.obj.id(),
            pos: 0,
        }
    }
}
