use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use endpoint::EndPointKey;
use twizzler_async::Task;
use twizzler_net::{ConnectionId, NmHandleManager};

use crate::client_request::handle_client_request;

mod arp;
mod client_request;
mod endpoint;
mod header;
mod link;
mod listen;
mod network;
mod nics;
mod route;
mod send;
mod transport;


pub struct HandleData {
    conn_id: AtomicU64,
    endpoints: Mutex<BTreeMap<ConnectionId, EndPointKey>>,
}

impl Default for HandleData {
    fn default() -> Self {
        Self {
            conn_id: AtomicU64::new(1),
            endpoints: Mutex::new(BTreeMap::new()),
        }
    }
}

impl HandleData {
    // generate new connection id by incrementing existing
    pub fn new_conn_id(&self) -> ConnectionId {
        self.conn_id.fetch_add(1, Ordering::SeqCst).into()
    }
    // return the endpointkey corresponding to the connection id
    pub fn get_endpoint_info(&self, id: ConnectionId) -> Option<EndPointKey> {
        self.endpoints.lock().unwrap().get(&id).cloned()
    }
    // add endpoing information for a connection into the handle data
    pub fn add_endpoint_info(&self, id: ConnectionId, info: EndPointKey) {
        self.endpoints.lock().unwrap().insert(id, info);
    }
}

pub type Handle = NmHandleManager<HandleData>;
pub type HandleRef = Arc<Handle>;

fn main() {
    println!("Hello from netmgr");

    // Initialize NICs
    nics::init();

    let num_threads = 2;
    println!("[NM] Spawning {} threads now.", num_threads);
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }
    // to count the number of handles created by the manager so far
   let mut num_handles: i32 = 0;
    println!("[NM] Done spawning {} threads now.", num_threads);   
    loop {
     println!("[NM] entered main loop.");   
        // Allocate and Initialize a network manager handle in the NETOBJ view
        // initialize a default HandleData
        let handle_data = HandleData::default();
        // create a new network manager handle with this data, in this view
        println!("[NM] initialized handledata.");   
        // this will wait for a handle request from a client before creating a new handle
        let nm_handle: Arc<NmHandleManager<HandleData>> = Arc::new(twizzler_net::server_open_nm_handle(handle_data).unwrap());
        println!("[NM] manager got new nm handle! {:?}", nm_handle);
        num_handles += 1; 
        println!("[NM] That was handle number {:?}", num_handles);

        // spawn a task for each new handle
        let _task = Task::spawn(async move {
                println!("[NM] network manager: waiting for request.");
            loop {
                // wait to get a request on this handle
                let request = nm_handle.receive().await;
                // if Ok, send request to hadlner.
                // if Error, break 
                if let Ok((id, req)) = request {
                    let _ = handle_client_request(&nm_handle, id, req).await;
                } else {
                    println!("[NM] got err {:?}", request);
                    break;
                }
                // if handle is terminated, close handle
                // if it died before termination, print error first
                if nm_handle.is_terminated() {
                    if nm_handle.is_dead() {
                        println!("[NM] got err");
                    }
                    break;
                }
                // listen for another request on this handle
            }
            println!("[NM] nm_handle was closed");
        })
        .detach();
    }
}
