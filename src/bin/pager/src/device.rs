use crate::memory::Page;

#[async_trait::async_trait]
pub trait BlockDevice {
    async fn write(&self, blocks: &[Block]) -> Result<(), BlockErr>;
    async fn read(&self, blocks: &[Block]) -> Result<(), BlockErr>;
    fn block_size(&self) -> Result<usize, BlockErr>;
    fn block_count(&self) -> Result<usize, BlockErr>;
}

pub enum BlockErr {
    IO,
    InvalidBlock,
}

pub struct Block {
    page: Page,
    num: u64,
}
