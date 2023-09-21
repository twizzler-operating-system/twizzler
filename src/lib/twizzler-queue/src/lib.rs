//! Provides a duplex send/completion queue, where each direction is multiple-producer/single-consumer.
//!
//! The core queue abstraction is built around two subqueues, each providing an MPSC
//! interface. These subqueues are stored in a single object, and so the verbs to interact with the
//! two subqueues are different.
//!
//! Generally a queue is thought of as providing a connection between a sender and a receiver, where
//! the sender sends requests to the receiver, and the receiver indications completion of requests.
//! Hence, one subqueue is the sending queue and the other is the completion queue. The subqueue
//! implementation is provided by the twizzler-queue-raw crate. This crate connects that crate to
//! the object system of Twizzler.
//!
//! The queues also provide hooks for asynchrony, allowing a given call to be non-blocking, and
//! methods to hook into for async executors to wait on events on a queue.
//!
//! Each subqueue sends a type T across the queue via byte-level copy. Internally, these objects are
//! held in a circular buffer with a maximum length specified on queue creation.

mod callback_queue;
mod queue;
mod sender_queue;

pub use callback_queue::CallbackQueueReceiver;
pub use queue::{Queue, QueueBase, QueueError, ReceiveFlags, SubmissionFlags};
pub use sender_queue::QueueSender;
