pub trait TwizzlerWaitable {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep;
    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep;
}
