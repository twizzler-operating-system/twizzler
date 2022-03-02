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

    pub fn into_inner(mut self) -> T {
        let handle = *self.handle.take().unwrap();
        Reactor::get().remove_wait_op(&self.source);
        handle
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

impl<T> Drop for Async<T> {
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = Reactor::get().remove_wait_op(&self.source);
            self.handle.take();
        }
    }
}

pub trait AsyncDuplexSetup {
    type ReadError: PartialEq;
    type WriteError: PartialEq;
    const READ_WOULD_BLOCK: Self::ReadError;
    const WRITE_WOULD_BLOCK: Self::WriteError;

    fn setup_read_sleep(&self) -> ThreadSyncSleep;
    fn setup_write_sleep(&self) -> ThreadSyncSleep;
}

pub struct AsyncDuplex<T> {
    read_source: Arc<Source>,
    write_source: Arc<Source>,
    handle: Option<Box<T>>,
}

impl<T: AsyncDuplexSetup> AsyncDuplex<T> {
    pub fn new(handle: T) -> Self {
        Self {
            read_source: Reactor::get().insert_wait_op(handle.setup_read_sleep()),
            write_source: Reactor::get().insert_wait_op(handle.setup_write_sleep()),
            handle: Some(Box::new(handle)),
        }
    }

    pub fn into_inner(mut self) -> T {
        let handle = *self.handle.take().unwrap();
        Reactor::get().remove_wait_op(&self.read_source);
        Reactor::get().remove_wait_op(&self.write_source);
        handle
    }

    pub fn get_ref(&self) -> &T {
        self.handle.as_ref().unwrap()
    }

    pub async fn read_with<R>(
        &self,
        op: impl FnMut(&T) -> Result<R, T::ReadError>,
    ) -> Result<R, T::ReadError> {
        let mut op = op;
        loop {
            let sleep_op = self.get_ref().setup_read_sleep();
            match op(self.get_ref()) {
                Err(e) if e == T::READ_WOULD_BLOCK => {}
                res => return res,
            }
            self.read_source.runnable(sleep_op).await;
        }
    }

    pub async fn write_with<R>(
        &self,
        op: impl FnMut(&T) -> Result<R, T::WriteError>,
    ) -> Result<R, T::WriteError> {
        let mut op = op;
        loop {
            let sleep_op = self.get_ref().setup_write_sleep();
            match op(self.get_ref()) {
                Err(e) if e == T::WRITE_WOULD_BLOCK => {}
                res => return res,
            }
            self.write_source.runnable(sleep_op).await;
        }
    }
}

impl<T> Drop for AsyncDuplex<T> {
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = Reactor::get().remove_wait_op(&self.read_source);
            let _ = Reactor::get().remove_wait_op(&self.write_source);
            self.handle.take();
        }
    }
}
