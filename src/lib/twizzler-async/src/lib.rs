//! Support for asynchronous programming on Twizzler. This crate provides executor functionality
//! along with support for async events and waiting, timers and timeouts, and a couple general
//! helper functions.
//!
//! # Executors
//! We provide three types of executors:
//!   1. block_on, which blocks until the future is completed.
//!   2. Thread-local, for futures that aren't Send.
//!   3. Global, which puts tasks in a global scheduling context for thread pools to handle.
//!
//! # Examples
//! The most basic way to run a future is:
//! ```
//! let result = block_on(async { /* some async code */ });
//! ```
//!
//! But this of course doesn't really make it possible to actually run things concurrently, since it
//! just waits for this single future. Instead, you probably want to use a real executor. The main
//! one you probably want is the global executor:
//! ```
//! let result = Task::spawn(async { /* some async code */ }).await;
//! ```
//! Now, this does assume that there is a thread that has called [crate::run], eg:
//! ```
//! let result = run(async { Task::spawn(async { /* some async code */ }).await });
//! ```
//!
//! Generally, though, if you want a thread pool, you can spawn a thread into a pool like this:
//! ```
//! std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
//! ```
//!
//! Then, later on, you can spawn a Task and await it. You can also detach a Task with .detach(),
//! which just places the thread on the runqueues and runs it without you having to await the result.
//!

mod async_source;
mod block_on;
mod event;
mod exec;
mod future;
mod reactor;
mod run;
mod task;
mod thread_local;
mod throttle;
mod timer;

pub use self::block_on::block_on;
pub use async_source::{Async, AsyncDuplex, AsyncDuplexSetup, AsyncSetup};
pub use future::{timeout_after, timeout_at, wait_for_first, FlagBlock};
pub use run::run;
pub use task::Task;
pub use timer::Timer;
