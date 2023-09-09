#![feature(return_position_impl_trait_in_trait)]

use std::{ffi::CStr, num::NonZeroUsize, sync::atomic::AtomicU32, time::Duration};

pub trait Runtime:
    ThreadRuntime + ObjectRuntime + CoreRuntime + RustFsRuntime + RustProcessRuntime
{
    // get random
}

pub struct ThreadSpawnArgs {
    pub stack: usize,
    pub start: usize,
    pub arg: usize,
}

pub trait ThreadRuntime {
    type InternalId: Copy;
    type SpawnError;

    const DEFAULT_MIN_STACK_SIZE: usize;

    fn available_parallelism(&self) -> NonZeroUsize;

    fn futex_wait(&self, futex: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool;
    fn futex_wake(&self, futex: &AtomicU32) -> bool;
    fn futex_wake_all(&self, futex: &AtomicU32);

    fn spawn(&self, args: ThreadSpawnArgs) -> Result<Self::InternalId, Self::SpawnError>;

    fn yield_now(&self);
    fn set_name(&self, name: &CStr);
    fn sleep(&self, duration: Duration);
    fn join(&self, id: Self::InternalId);
}

pub trait ObjectRuntime {}

pub trait CoreRuntime {
    fn default_allocator(&self) -> impl core::alloc::GlobalAlloc;
    fn pre_main_hook(&self) {}
    fn post_main_hook(&self) {}
    fn exit(&self, code: i32);
    fn abort(&self);
}

pub struct BasicAux {
    pub argc: usize,
    pub args: *const *const i8,
    pub env: *const *const i8,
}

pub struct BasicReturn {}

pub trait RustFsRuntime {}

pub trait RustProcessRuntime: RustStdioRuntime {}

pub trait RustStdioRuntime {
    type Stdin: IoRead;
    type Stdout: IoWrite;
    type Stderr: IoWrite;

    fn panic_output(&self) -> impl IoWrite;
}

pub trait IoRead {
    type ReadErrorType;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::ReadErrorType>;
}

pub trait IoWrite {
    type WriteErrorType;
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::WriteErrorType>;
    fn flush(&mut self) -> Result<(), Self::WriteErrorType>;
}

pub trait RustTimeRuntime {
    type InstantType: RustInstant + Into<Duration>;
    type SystemTimeType: Into<Duration>;

    fn get_monotonic(&self) -> Self::InstantType;
    fn get_system_time(&self) -> Self::SystemTimeType;
}

pub trait RustInstant {
    fn actually_monotonic(&self) -> bool;
}

pub type LibstdEntry = fn(aux: BasicAux) -> BasicReturn;
