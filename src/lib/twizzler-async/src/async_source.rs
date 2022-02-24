use std::sync::Arc;

use twizzler_abi::syscall::ThreadSyncSleep;

use crate::reactor::{Reactor, Source};

pub trait AsyncSetup {
    type Error: PartialEq;
    const WOULD_BLOCK: Self::Error;

    fn setup_sleep(&self) -> ThreadSyncSleep;
}

pub struct Async<T> {
    source: Arc<Source>,
    handle: Option<Box<T>>,
}

impl<T: AsyncSetup> Async<T> {
    pub fn new(handle: T) -> Self {
        Self {
            source: Reactor::get().insert_wait_op(handle.setup_sleep()),
            handle: Some(Box::new(handle)),
        }
    }

    pub fn get_ref(&self) -> &T {
        self.handle.as_ref().unwrap()
    }

    pub async fn run_with<R>(
        &self,
        op: impl FnMut(&T) -> Result<R, T::Error>,
    ) -> Result<R, T::Error> {
        let mut op = op;
        loop {
            let sleep_op = self.get_ref().setup_sleep();
            match op(self.get_ref()) {
                Err(e) if e == T::WOULD_BLOCK => {}
                res => return res,
            }
            self.source.runnable(sleep_op).await;
        }
    }
}
