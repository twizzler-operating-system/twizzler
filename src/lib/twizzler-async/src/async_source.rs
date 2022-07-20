use std::sync::Arc;

use twizzler_abi::syscall::ThreadSyncSleep;

use crate::reactor::{Reactor, Source};

/// Implement setting up externally signaled asynchronous events for the async runner to wait for,
/// in the case where there is a single "runnable" abstraction for this object.
pub trait AsyncSetup {
    /// The error type returned by any closures run.
    type Error: PartialEq;
    /// The specific variant of the error type that indicates that an operation would block.
    const WOULD_BLOCK: Self::Error;

    /// Return a thread sync sleep operation specification for this handle.
    fn setup_sleep(&self) -> ThreadSyncSleep;
}

#[derive(Debug)]
/// A wrapper type around some "handle" that we want to perform asynchronous operations on, where
/// that handle must implement [AsyncSetup].
pub struct Async<T> {
    source: Arc<Source>,
    handle: Option<Box<T>>,
}

impl<T: AsyncSetup> Async<T> {
    /// Construct a new Async<T>.
    pub fn new(handle: T) -> Self {
        Self {
            source: Reactor::get().insert_wait_op(handle.setup_sleep()),
            handle: Some(Box::new(handle)),
        }
    }

    /// Return a reference to the underlying handle.
    pub fn get_ref(&self) -> &T {
        self.handle.as_ref().unwrap()
    }

    /// Consume this Async<T> and return the handle.
    pub fn into_inner(mut self) -> T {
        let handle = *self.handle.take().unwrap();
        Reactor::get().remove_wait_op(&self.source);
        handle
    }

    /// Asynchronously run an operation that will sleep if not ready. The closure to run must return
    /// `Result<_, T::Error>`, and should return `Err(T::WOULD_BLOCK)` if the operation is not ready.
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

/// Implement setting up externally signaled asynchronous events for the async runner to wait for,
/// in the case where there is a duplex mode for reading and writing to this object, each of which
/// could fail with some "would block" error.
pub trait AsyncDuplexSetup {
    /// The error type returned by read operations.
    type ReadError: PartialEq;
    /// The error type returned by write operations.
    type WriteError: PartialEq;

    /// The specific variant of the error type that indicates that a read operation would block.
    const READ_WOULD_BLOCK: Self::ReadError;
    /// The specific variant of the error type that indicates that a write operation would block.
    const WRITE_WOULD_BLOCK: Self::WriteError;

    /// Return a thread sync sleep operation specification for reading from this handle.
    fn setup_read_sleep(&self) -> ThreadSyncSleep;
    /// Return a thread sync sleep operation specification for writing to this handle.
    fn setup_write_sleep(&self) -> ThreadSyncSleep;
}

/// A wrapper type around some "handle" that we want to perform asynchronous operations on, where
/// that handle must implement [AsyncDuplexSetup].
pub struct AsyncDuplex<T> {
    read_source: Arc<Source>,
    write_source: Arc<Source>,
    handle: Option<Box<T>>,
}

impl<T: AsyncDuplexSetup> AsyncDuplex<T> {
    /// Construct a new `Async<T>`.
    pub fn new(handle: T) -> Self {
        Self {
            read_source: Reactor::get().insert_wait_op(handle.setup_read_sleep()),
            write_source: Reactor::get().insert_wait_op(handle.setup_write_sleep()),
            handle: Some(Box::new(handle)),
        }
    }

    /// Consume the wrapper and return the underlying handle.
    pub fn into_inner(mut self) -> T {
        let handle = *self.handle.take().unwrap();
        Reactor::get().remove_wait_op(&self.read_source);
        Reactor::get().remove_wait_op(&self.write_source);
        handle
    }

    /// Return a reference to the underlying handle.
    pub fn get_ref(&self) -> &T {
        self.handle.as_ref().unwrap()
    }

    /// Asynchronously run a read-like operation that will sleep if not ready. The closure to run must return
    /// `Result<_, T::ReadError>`, and should return `Err(T::READ_WOULD_BLOCK)` if the operation is not ready.
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

    /// Asynchronously run a write-like operation that will sleep if not ready. The closure to run must return
    /// `Result<_, T::WriteError>`, and should return `Err(T::WRITE_WOULD_BLOCK)` if the operation is not ready.
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
