use twizzler::{object::ObjID, Result};
use twizzler_abi::pager::{ObjectRange, PhysRange};
pub trait MemStore {
    fn set_config_id(&self, id: ObjID) -> Result<()>;
    fn get_config_id(&self) -> Result<ObjID>;

    fn get_map(&self, id: ObjID, range: ObjectRange) -> Result<impl Iterator<Item = PhysRange>>;

    fn flush(&self) -> Result<()> {
        Ok(())
    }
}
