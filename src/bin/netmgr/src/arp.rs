#![allow(dead_code)]
#![allow(unused_imports)]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use twizzler_async::{timeout_after, FlagBlock};
use twizzler_net::addr::Ipv4Addr;

use crate::ethernet::EthernetAddr;

const ARP_TIMEOUT: Duration = Duration::from_millis(8000);

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ArpInfo {
    eth_addr: EthernetAddr,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct ArpKey {
    addr: Ipv4Addr,
}

struct ArpTableInner {
    entries: Mutex<BTreeMap<ArpKey, ArpInfo>>,
    flag: FlagBlock,
}

struct ArpTable {
    inner: Arc<ArpTableInner>,
}

lazy_static::lazy_static! {
    static ref ARP_TABLE: ArpTable = ArpTable::new();
}

impl ArpTable {
    fn new() -> Self {
        Self {
            inner: Arc::new(ArpTableInner {
                entries: Mutex::new(BTreeMap::new()),
                flag: FlagBlock::new(),
            }),
        }
    }

    async fn lookup(&self, dst: ArpKey) -> ArpInfo {
        loop {
            let entries = self.inner.entries.lock().unwrap();
            if let Some(res) = entries.get(&dst) {
                return *res;
            }

            println!("didn't find arp entry");
            // a gotcha: you have to get the future here before releasing the entries mutex, and
            // then you must await on it _after_ the lock has been released.
            let fut = self.inner.flag.wait();
            println!("future created");
            drop(entries);
            println!("firing request");
            self.fire_arp_request(dst);
            println!("awaiting completion");
            fut.await;
        }
    }

    fn fire_arp_request(&self, _dst: ArpKey) {
        println!("fire arp request");
    }

    fn add_entry(&self, key: ArpKey, info: ArpInfo) {
        self.inner.entries.lock().unwrap().insert(key, info);
        self.inner.flag.signal_all();
    }
}

pub async fn lookup_arp_info(key: ArpKey) -> Result<Option<ArpInfo>, ()> {
    Ok(twizzler_async::timeout_after(ARP_TABLE.lookup(key), ARP_TIMEOUT).await)
}
