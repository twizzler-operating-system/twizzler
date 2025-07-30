use super::MemDevice;

pub struct VirtioMem {
    info: u32,
}

impl MemDevice for VirtioMem {
    fn get_physical(
        &self,
        start: u64,
        len: u64,
    ) -> twizzler::Result<twizzler_abi::pager::PhysRange> {
        todo!()
    }

    fn page_size() -> u64
    where
        Self: Sized,
    {
        0x1000
    }

    fn flush(&self, start: u64, len: u64) -> twizzler::Result<()> {
        Ok(())
    }
}
