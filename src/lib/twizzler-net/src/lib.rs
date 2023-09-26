//! The twizzler-net crate.
//!
//! The purpose of this crate is to provide:
//!
//!  - Networking data structures
//!  - Basic functions for sending and receiving packets
//!  - A vanilla TCP/IP stack
//! 
//! # Design
//!     - This library talks to a NIC device driver through a common interface
//!     - 
//!     - The library notifies the driver of packets to be sent in the transmit queue

use std::sync::atomic::{AtomicU64, Ordering};

use twizzler_abi::{
    marker::BaseType,
    syscall::{
        ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
        ThreadSyncWake,
    },
};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

#[cfg(feature = "manager")]
use twizzler_abi::syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags};

pub mod addr;
pub mod buffer;
mod nm_handle;
mod req;
mod rx_req;
mod tx_req;
pub use req::{CloseInfo, ConnectionId, PacketData};
pub use rx_req::{Connection, RxCompletion, RxRequest};
pub use tx_req::{ListenFlags, ListenInfo, TxCompletion, TxCompletionError, TxRequest};

pub use nm_handle::{open_nm_handle, NmHandle};

#[cfg(feature = "manager")]
pub use nm_handle::{server_open_nm_handle, NmHandleManager};

struct Rendezvous {
    ready: AtomicU64,
    tx_buf: ObjID,
    rx_buf: ObjID,
    tx_queue: ObjID,
    rx_queue: ObjID,
    client_name: [u8; 256],
    client_id: u64,
}

impl BaseType for Rendezvous {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(
        twizzler_abi::marker::BaseVersion,
        twizzler_abi::marker::BaseTag,
    )] {
        todo!()
    }
}

#[allow(dead_code)]
const NM_NOT_READY: u64 = 0;
const NM_READY_NO_DATA: u64 = 1;
const NM_READY_DATA: u64 = 2;
const CLIENT_TAKING: u64 = 3;
const CLIENT_DONE: u64 = 4;

#[derive(Debug)]
pub struct NmOpenObjects {
    tx_buf: ObjID,
    rx_buf: ObjID,
    tx_queue: ObjID,
    rx_queue: ObjID,
    client_id: u64,
    #[allow(dead_code)]
    client_name: [u8; 256],
}

fn wait_until_eq(pt: &AtomicU64, val: u64) {
    loop {
        let cur = pt.load(Ordering::SeqCst);
        if cur == val {
            return;
        }
        let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(pt as *const AtomicU64),
            cur,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    }
}

fn wait_until_neq(pt: &AtomicU64, val: u64) {
    loop {
        let cur = pt.load(Ordering::SeqCst);
        if cur != val {
            return;
        }
        let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(pt as *const AtomicU64),
            cur,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    }
}

fn write_wake(pt: &AtomicU64, val: u64) {
    pt.store(val, Ordering::SeqCst);
    let op = ThreadSync::new_wake(ThreadSyncWake::new(
        ThreadSyncReference::Virtual(pt as *const AtomicU64),
        usize::MAX,
    ));
    let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
}

#[cfg(feature = "manager")]
fn new_obj() -> ObjID {
    let create = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    twizzler_abi::syscall::sys_object_create(create, &[], &[]).unwrap()
}

#[cfg(feature = "manager")]
fn new_q<S: Copy, C: Copy>() -> ObjID {
    use twizzler_object::CreateSpec;
    use twizzler_queue::Queue;
    let create = CreateSpec::new(LifetimeType::Volatile, BackingType::Normal);
    let q: Queue<S, C> = Queue::create(&create, 64, 64).unwrap();
    q.object().id()
}

pub fn wait_until_network_manager_ready(rid: ObjID) {
    let obj = Object::<Rendezvous>::init_id(
        rid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let rendezvous = obj.base().unwrap();
    wait_until_neq(&rendezvous.ready, 0);
}

pub fn is_network_manager_ready(rid: ObjID) -> bool {
    let obj = Object::<Rendezvous>::init_id(
        rid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    let rendezvous = obj.base().unwrap();
    rendezvous.ready.load(Ordering::SeqCst) != 0
}

#[cfg(feature = "manager")]
fn server_rendezvous(rid: ObjID) -> NmOpenObjects {
    // initialize the slot counter for this view
    static ID_COUNTER: AtomicU64 = AtomicU64::new(1);
    // initialize a new rendezvous object in the given view object
    let obj = Object::<Rendezvous>::init_id(
        rid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();
    // get a mutable pointer to the base of the new rendezvous object
    let mut rendezvous = unsafe { obj.base_mut_unchecked() };
   // if rendezvous object isn't ready, mark it ready
    println!("[NM] waiting for rendezvous object to be ready.");
    if rendezvous.ready.load(Ordering::SeqCst) == NM_NOT_READY {
        write_wake(&rendezvous.ready, NM_READY_NO_DATA);
    }
        // wait for the mark to be set
    wait_until_eq(&rendezvous.ready, NM_READY_NO_DATA);
    // initialize a new blank NmOpenObjects instance
    let mut o = NmOpenObjects {
        tx_buf: new_obj(),
        rx_buf: new_obj(),
        // default queue size: 64 requests or completions
        tx_queue: new_q::<TxRequest, TxCompletion>(),
        rx_queue: new_q::<RxRequest, RxCompletion>(),
        client_id: 0,
        client_name: [0; 256],
    };
    // initialize the base object of the Rendezvous object, obj, to be an NmOpenObjects instance
    rendezvous.tx_buf = o.tx_buf;
    rendezvous.rx_buf = o.rx_buf;
    rendezvous.tx_queue = o.tx_queue;
    rendezvous.rx_queue = o.rx_queue;
    // increment slot id counter
    let id = ID_COUNTER.fetch_add(1, Ordering::SeqCst);
    // use the previous id for the rendezvous object
    rendezvous.client_id = id;
    rendezvous.client_name = [0; 256];
    //  wait for the rendezvous object to be ready
    write_wake(&rendezvous.ready, NM_READY_DATA);
    // wait for client to make a request and fill in the rendezvous.client_name
    println!("[NM] Waiting for a client with a blank rendezvous object ready.");
        wait_until_eq(&rendezvous.ready, CLIENT_DONE);
    println!("[NM] client has taken a rendezvous object.");
    o.client_id = id;
    o.client_name.copy_from_slice(&rendezvous.client_name);
    write_wake(&rendezvous.ready, NM_READY_NO_DATA);
    o
}


/// Return a NmOpenObjects handle from the network manager in the given view
/// set the client_name of the object
fn client_rendezvous(rid: ObjID, client_name: &str) -> NmOpenObjects {
    // initialize rendezvous object in view
    let obj = Object::<Rendezvous>::init_id(
        rid,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .unwrap();

    // retrieve a mutable pointer to the Rendezvous struct inside the object
    let rendezvous = unsafe { obj.base_mut_unchecked() };

   // wait for network manager to have a handle ready, then take it
    // set rendezvous.ready to CLIENT_TAKING
    // this can only be set if it is in the NM_READY_DATA state
    loop {
        wait_until_eq(&rendezvous.ready, NM_READY_DATA);
        if rendezvous.ready.swap(CLIENT_TAKING, Ordering::SeqCst) == NM_READY_DATA {
            break;
        }
    }
    // now this thread has exclusive access to the rendezvous object

    // construct a NmOpenObjects struct named client_name and return it
    // truncate the name to 255 bytes if needed before setting rendezvous.client_name
    let name_len = std::cmp::min(255, client_name.len());

    //let bytes = client_name.as_bytes();  // convert name to array of bytes
    //let name = &bytes[0..name_len-1]; //truncate to 255 bytes if needed
    rendezvous.client_name[0..name_len-1].copy_from_slice(&client_name.as_bytes()[0..name_len-1]);
    let o = NmOpenObjects {
        tx_buf: rendezvous.tx_buf,
        rx_buf: rendezvous.rx_buf,
        tx_queue: rendezvous.tx_queue,
        rx_queue: rendezvous.rx_queue,
        client_id: rendezvous.client_id,
        client_name: rendezvous.client_name,
    };
    // before returning, release lock on rendezvous object
    write_wake(&rendezvous.ready, CLIENT_DONE);
    // this lets server create a new handle for the next client
    // return the network handle
    o
}
