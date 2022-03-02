use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use twizzler_async::{timeout_after, FlagBlock};

struct ArpTableInner {
    entries: Mutex<BTreeMap<u32, u32>>,
    flag: FlagBlock,
}

struct ArpTable {
    inner: Arc<ArpTableInner>,
}

impl ArpTable {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArpTableInner {
                entries: Mutex::new(BTreeMap::new()),
                flag: FlagBlock::new(),
            }),
        }
    }

    pub async fn lookup(&self, dst: u32) -> u32 {
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

    fn fire_arp_request(&self, dst: u32) {
        println!("fire arp request");
        let inner = self.inner.clone();
        std::thread::spawn(move || {
            println!("arp request sent...");
            twizzler_async::block_on(async {
                twizzler_async::Timer::after(Duration::from_millis(3000)).await;
            });
            println!("inserting entry and signaling");
            inner.entries.lock().unwrap().insert(dst, 1234);
            inner.flag.signal_all();
        });
    }

    pub fn add_entry(&self, a: u32, b: u32) {
        self.inner.entries.lock().unwrap().insert(a, b);
        self.inner.flag.signal_all();
    }
}

pub fn test_arp() {
    let arp = ArpTable::new();
    twizzler_async::run(async {
        let ent = timeout_after(arp.lookup(1), Duration::from_millis(2000)).await;
        println!("arp got: {:?}", ent);
    });
}
