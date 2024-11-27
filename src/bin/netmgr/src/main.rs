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
    pub fn new_conn_id(&self) -> ConnectionId {
        self.conn_id.fetch_add(1, Ordering::SeqCst).into()
    }

    pub fn get_endpoint_info(&self, id: ConnectionId) -> Option<EndPointKey> {
        self.endpoints.lock().unwrap().get(&id).cloned()
    }

    pub fn add_endpoint_info(&self, id: ConnectionId, info: EndPointKey) {
        self.endpoints.lock().unwrap().insert(id, info);
    }
}

pub type Handle = NmHandleManager<HandleData>;
pub type HandleRef = Arc<Handle>;

extern crate twizzler_minruntime;
fn main() {
    println!("Hello from netmgr");

    nics::init();

    let num_threads = 1;
    for _ in 0..num_threads {
        std::thread::spawn(|| twizzler_async::run(std::future::pending::<()>()));
    }

    loop {
        let handle_data = HandleData::default();
        let nm_handle = Arc::new(twizzler_net::server_open_nm_handle(handle_data).unwrap());
        println!("manager got new nm handle! {:?}", nm_handle);
        let _task = Task::spawn(async move {
            loop {
                let request = nm_handle.receive().await;
                if let Ok((id, req)) = request {
                    let _ = handle_client_request(&nm_handle, id, req).await;
                } else {
                    println!("got err {:?}", request);
                    break;
                }

                if nm_handle.is_terminated() {
                    if nm_handle.is_dead() {
                        println!("got err");
                    }
                    break;
                }
            }
            println!("nm_handle was closed");
        })
        .detach();
    }
}
