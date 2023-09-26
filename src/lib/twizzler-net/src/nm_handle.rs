use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use twizzler_queue::{CallbackQueueReceiver, QueueBase, QueueError, QueueSender, SubmissionFlags};

use crate::{
    buffer::{BufferBase, BufferController, ManagedBuffer},
    client_rendezvous,
    req::PacketData,
    rx_req::{RxCompletion, RxRequest},
    tx_req::{TxCompletion, TxRequest},
};

#[cfg(feature = "manager")]
use crate::server_rendezvous;

/// A structure containing the transfer and receive queues and buffers
struct NmHandleObjects {
    tx_queue: Object<QueueBase<TxRequest, TxCompletion>>,
    rx_queue: Object<QueueBase<RxRequest, RxCompletion>>,
    #[allow(dead_code)]
    tx_buf: Object<BufferBase>,
    #[allow(dead_code)]
    rx_buf: Object<BufferBase>,
}

const DEAD: u64 = 1;
const CLOSED: u64 = 2;

/// The basic network communication handle that has all the structures
/// needed to perform network transmits and receives
pub struct NmHandle {
    _objs: NmHandleObjects,
    handler: CallbackQueueReceiver<RxRequest, RxCompletion>,
    sender: QueueSender<TxRequest, TxCompletion>,
    tx_bc: BufferController,
    rx_bc: BufferController,
    flags: AtomicU64,
    client_name: String,
    client_id: u64,
}

// a network handle plus additional data field
// This is used by the network manager to cache connections
#[cfg(feature = "manager")]
pub struct NmHandleManager<T> {
    _objs: NmHandleObjects,
    handler: CallbackQueueReceiver<TxRequest, TxCompletion>,
    sender: QueueSender<RxRequest, RxCompletion>,
    tx_bc: BufferController,
    rx_bc: BufferController,
    flags: AtomicU64,
    client_name: String,
    client_id: u64,
    data: T,
}

impl NmHandle {
    pub async fn handle<'a, F, Fut>(self: &'a Arc<NmHandle>, f: F) -> Result<(), QueueError>
    where
        F: Fn(&'a Arc<NmHandle>, u32, RxRequest) -> Fut,
        Fut: Future<Output = RxCompletion>,
    {
        self.handler.handle(move |id, req| f(self, id, req)).await
    }

    pub async fn submit(&self, req: TxRequest) -> Result<TxCompletion, QueueError> {
        self.sender.submit_and_wait(req).await
    }

    pub fn submit_no_wait(&self, req: TxRequest) {
        self.sender.submit_no_wait(req, SubmissionFlags::NON_BLOCK);
    }

    /*
    pub fn tx_buffer_controller(&self) -> &BufferController {
        &self.tx_bc
    }

    pub fn rx_buffer_controller(&self) -> &BufferController {
        &self.rx_bc
    }
    */

    pub fn allocatable_buffer_controller(&self) -> &BufferController {
        &self.tx_bc
    }

    pub fn set_dead(&self) {
        self.flags.fetch_or(DEAD, Ordering::SeqCst);
    }

    pub fn is_dead(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & DEAD != 0
    }

    pub fn set_closed(&self) {
        self.flags.fetch_or(CLOSED, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & CLOSED != 0
    }

    pub fn is_terminated(&self) -> bool {
        self.is_closed() || self.is_dead()
    }

    pub fn get_incoming_buffer(&self, pd: PacketData) -> ManagedBuffer {
        ManagedBuffer::new_unowned(&self.rx_bc, pd.buffer_idx, pd.buffer_len as usize)
    }

    pub fn id(&self) -> u64 {
        self.client_id
    }

    pub fn client_name(&self) -> &str {
        &self.client_name
    }
}

#[cfg(feature = "manager")]
impl<T> NmHandleManager<T> {
    pub fn data(&self) -> &T {
        &self.data
    }

    pub async fn receive(&self) -> Result<(u32, TxRequest), QueueError> {
        if self.is_terminated() {
            Err(QueueError::Unknown)
        } else {
            self.handler.receive().await
        }
    }

    pub async fn complete(&self, id: u32, reply: TxCompletion) -> Result<(), QueueError> {
        self.handler.complete(id, reply).await
    }

    pub async fn submit(&self, req: RxRequest) -> Result<RxCompletion, QueueError> {
        if self.is_terminated() {
            return Err(QueueError::Unknown);
        }
        self.sender.submit_and_wait(req).await
    }

    pub fn submit_no_wait(&self, req: RxRequest) {
        self.sender.submit_no_wait(req, SubmissionFlags::NON_BLOCK);
    }

    /*
    pub fn tx_buffer_controller(&self) -> &BufferController {
        &self.tx_bc
    }

    pub fn rx_buffer_controller(&self) -> &BufferController {
        &self.rx_bc
    }
    */

    pub fn allocatable_buffer_controller(&self) -> &BufferController {
        &self.rx_bc
    }

    pub fn set_dead(&self) {
        self.flags.fetch_or(DEAD, Ordering::SeqCst);
    }

    pub fn is_dead(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & DEAD != 0
    }

    pub fn set_closed(&self) {
        self.flags.fetch_or(CLOSED, Ordering::SeqCst);
    }

    pub fn is_closed(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & CLOSED != 0
    }

    pub fn is_terminated(&self) -> bool {
        self.is_closed() || self.is_dead()
    }

    pub fn get_incoming_buffer(&self, pd: PacketData) -> ManagedBuffer {
        ManagedBuffer::new_unowned(&self.tx_bc, pd.buffer_idx, pd.buffer_len as usize)
    }

    pub fn id(&self) -> u64 {
        self.client_id
    }

    pub fn client_name(&self) -> &str {
        &self.client_name
    }
}

impl Drop for NmHandle {
    fn drop(&mut self) {
        println!("dropping nm handle");
        if !self.is_dead() {
            self.submit_no_wait(TxRequest::Close);
        }
    }
}

#[cfg(feature = "manager")]
impl<T> Drop for NmHandleManager<T> {
    fn drop(&mut self) {
        println!("dropping nm handle manager");
        if !self.is_dead() {
            self.submit_no_wait(RxRequest::Close);
        }
    }
}

impl core::fmt::Debug for NmHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NmHandle")
            .field("client_id", &self.client_id)
            .field("client_name", &self.client_name)
            .field("flags", &self.flags)
            .finish()
    }
}

#[cfg(feature = "manager")]
impl<T> core::fmt::Debug for NmHandleManager<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NmHandleManager")
            .field("client_id", &self.client_id)
            .field("client_name", &self.client_name)
            .field("flags", &self.flags)
            .finish()
    }
}

/// return an NmHandle with the given client_name
pub fn open_nm_handle(client_name: &str) -> Option<NmHandle> {
    // read the value of the NETOBJ environment variable back into an ObjID
    // This is the network view
    let id = std::env::var("NETOBJ").ok()?;
    let id = id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);

    // get an available network handle from network manager
    let objs = client_rendezvous(id, client_name);
    let client_id = objs.client_id;

    // initialize a NmHandle for use
    // The initializations below of the elements of the NmHandle are repeated on the server side
    // A race condition is avoided in the init function by using the get() function
    // which returns an atomic reference counted pointer to an existing object

    /*Begin Repeated code */
    // start with a blank NmHandleObjects 
    let objs = NmHandleObjects {
        tx_queue: Object::init_id(
            objs.tx_queue,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
        rx_queue: Object::init_id(
            objs.rx_queue,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
        tx_buf: Object::init_id(
            objs.tx_buf,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
        rx_buf: Object::init_id(objs.rx_buf, Protections::READ, ObjectInitFlags::empty()).ok()?,
    };
    // sender, handler, transfer and receive buffer controllers
    let sender = QueueSender::new(objs.tx_queue.clone().into());
    let handler = CallbackQueueReceiver::new(objs.rx_queue.clone().into());
    let tx_bc = BufferController::new(false, true, objs.tx_buf.clone());
    let rx_bc = BufferController::new(false, false, objs.rx_buf.clone());
    /*End repeated code */

    // Construct an NmHandle and return it
    let handle = NmHandle {
        _objs: objs,
        handler,
        sender,
        tx_bc,
        rx_bc,
        flags: AtomicU64::new(0),
        client_name: client_name.to_owned(),
        client_id,
    };
    Some(handle)
}

#[cfg(feature = "manager")]
// Open a Network Manager handle with the given handle data
// after a client has requested a handle
pub fn server_open_nm_handle<T>(data: T) -> Option<NmHandleManager<T>> {
    use std::ffi::CStr;

    // import 128 bit ID of the networking view
    let id = std::env::var("NETOBJ").ok()?;
    let id = id
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("failed to parse object ID string {}", id));
    let id = ObjID::new(id);
     println!("[NM] ObjID for NETOBJ view is {:?}.", id);
    // allocate a NmOpenObjects structure to a new slice in the NETOBJ view
    // wait for a client request
    let objs = server_rendezvous(id);
     println!("[NM] Allocated an NmOpenObjects struct.");
         // fill in the object name to be client_name
    let client_name = CStr::from_bytes_with_nul(
        &objs.client_name[0..=objs.client_name.iter().position(|x| *x == 0).unwrap_or(0)],
    )
    .unwrap_or_else(|_| CStr::from_bytes_with_nul(&[0]).unwrap());
    let client_name = client_name.to_str().unwrap_or("").to_owned();
    println!("Client name for netobj nmhandle is: {:?}", client_name);
    let client_id = objs.client_id;
    // initialize the elements of the NmHandleObjects
    let objs = NmHandleObjects {
        // the transfer queue
        tx_queue: Object::init_id(
            objs.tx_queue,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
        // the receive queue
        rx_queue: Object::init_id(
            objs.rx_queue,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
        // the transfer buffer
        tx_buf: Object::init_id(objs.tx_buf, Protections::READ, ObjectInitFlags::empty()).ok()?,
        // the receive buffer
        rx_buf: Object::init_id(
            objs.rx_buf,
            Protections::READ | Protections::WRITE,
            ObjectInitFlags::empty(),
        )
        .ok()?,
    };
    let sender = QueueSender::new(objs.rx_queue.clone().into());
    let handler = CallbackQueueReceiver::new(objs.tx_queue.clone().into());
    let tx_bc = BufferController::new(true, true, objs.tx_buf.clone());
    let rx_bc = BufferController::new(true, false, objs.rx_buf.clone());
    // initialize a new NmHandleManager
    let handle = NmHandleManager {
        _objs: objs,
        handler,
        sender,
        tx_bc,
        rx_bc,
        flags: AtomicU64::new(0),
        client_name,
        client_id,
        data,
    };
    println!("[NM] Returning NmHandleManager for object NETOBJ.");
    Some(handle)
}
