use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use twizzler_async::Task;

use crate::link::{
    ethernet::{handle_incoming_ethernet_packets, EthernetAddr},
    nic::NetworkInterface,
};

mod loopback;

lazy_static::lazy_static! {
    static ref NIC_MANAGER: NicManager = NicManager::new();
}

struct NicManagerInner {
    nics: BTreeMap<EthernetAddr, Arc<dyn NetworkInterface + Sync + Send>>,
}

struct NicManager {
    inner: Mutex<NicManagerInner>,
}

impl NicManager {
    fn new() -> Self {
        Self {
            inner: Mutex::new(NicManagerInner {
                nics: BTreeMap::new(),
            }),
        }
    }
}

pub fn init() {
    let mut inner = NIC_MANAGER.inner.lock().unwrap();
    let lo = Arc::new(loopback::Loopback::new());
    inner.nics.insert(lo.get_ethernet_addr(), lo.clone());
    Task::spawn(async move {
        loop {
            let recv = lo.recv_ethernet().await;
            if let Ok(recv) = recv {
                handle_incoming_ethernet_packets(&recv).await;
            } else {
                eprintln!("loopback recv thread encountered an error: {:?}", recv);
                break;
            }
        }
    })
    .detach();
}

pub fn lookup_nic(addr: &EthernetAddr) -> Option<Arc<dyn NetworkInterface + Send + Sync>> {
    let inner = NIC_MANAGER.inner.lock().unwrap();
    inner.nics.get(addr).cloned()
}
