use futures::{stream::select_all, FutureExt, Stream};
use twizzler_abi::{
    device::{BusType, DeviceInterruptFlags, DeviceRepr},
    kso::KactionError,
};
use twizzler_async::{Async, AsyncSetup};

use super::Device;

#[derive(Debug)]
pub(crate) struct InterruptDataInner {
    pub repr: *const DeviceRepr,
    pub inum: usize,
}

impl AsyncSetup for InterruptDataInner {
    type Error = bool;

    const WOULD_BLOCK: Self::Error = true;

    fn setup_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let repr = unsafe { self.repr.as_ref().unwrap_unchecked() };
        repr.setup_interrupt_sleep(self.inum)
    }
}

#[derive(Debug)]
pub(crate) struct InterruptData {
    pub src: Async<InterruptDataInner>,
}

pub struct DeviceInterrupt<'a> {
    device: &'a Device,
    index: usize,
    device_vector: u32,
}

pub enum InterruptAllocationError {
    NoMoreInterrupts,
    Unsupported,
    KernelError(KactionError),
}

impl Device {
    pub fn next_interrupt(
        &self,
        inum: usize,
    ) -> impl Stream<Item = Result<(usize, u64), bool>> + '_ {
        let repr = self.repr();
        self.ints[inum]
            .src
            .run_with(move |inner| {
                repr.check_for_interrupt(inum)
                    .ok_or(true)
                    .map(|x| (inner.inum, x))
            })
            .into_stream()
    }

    pub fn next_any_interrupt(&self) -> impl Stream<Item = Result<(usize, u64), bool>> + '_ {
        select_all(
            self.ints
                .iter()
                .map(|i| Box::pin(self.next_interrupt(i.src.get_ref().inum))),
        )
    }

    pub fn setup_interrupt(&self) -> Result<DeviceInterrupt<'_>, InterruptAllocationError> {
        let repr = self.repr_mut();
        let inum = {
            let mut bv = self.taken_ints.lock().unwrap();
            let inum = bv
                .first_zero()
                .ok_or(InterruptAllocationError::NoMoreInterrupts)?;
            bv.set(inum, true);
            inum
        };

        let (vec, devint) = match self.bus_type() {
            BusType::Pcie => self.allocate_interrupt(inum)?,
            _ => return Err(InterruptAllocationError::Unsupported),
        };
        repr.register_interrupt(inum, vec, DeviceInterruptFlags::empty());
        Ok(DeviceInterrupt {
            device: self,
            index: inum,
            device_vector: devint,
        })
    }
}
