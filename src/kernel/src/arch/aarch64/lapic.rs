use crate::{
    clock::Nanoseconds,
    interrupt::Destination,
    memory::VirtAddr,
};

pub fn schedule_oneshot_tick(_time: Nanoseconds) {
    todo!()
}

pub fn send_ipi(_dest: Destination, _vector: u32) {
    todo!("send an ipi")
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(_cpu: u32, _tcb_base: VirtAddr, _kernel_stack: *mut u8) {
    todo!("start up a cpu")
}
