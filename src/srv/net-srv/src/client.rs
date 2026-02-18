use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
};

use smoltcp::{
    phy::{Device, RxToken, TxToken},
    time::Instant,
    wire::{EthernetFrame, PrettyPrinter},
};
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync};
use twizzler_net::NetServer;
use virtio_net::TxBuffer;

use crate::NETINFO;

pub struct Client {
    pub ep: Mutex<NetServer>,
    jh: OnceLock<JoinHandle<()>>,
    pub active: AtomicBool,
    pub ports: Mutex<HashSet<u16>>,
}

impl Client {
    pub fn new(ep: NetServer) -> Arc<Self> {
        let client = Arc::new(Client {
            ep: Mutex::new(ep),
            jh: OnceLock::new(),
            active: AtomicBool::new(true),
            ports: Mutex::new(HashSet::new()),
        });
        let _client = client.clone();
        let jh = std::thread::spawn(move || client_thread(_client));
        client.jh.set(jh).unwrap();
        client
    }

    fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

fn client_thread(client: Arc<Client>) {
    let device = NETINFO.get().unwrap().device.clone();
    let tx_po = client.ep.lock().unwrap().client_tx_packet_object().clone();
    while client.active() {
        let mut ep = client.ep.lock().unwrap();
        while let Some((rx, _tx)) = ep.receive(Instant::now()) {
            let packet = rx.packet;
            rx.consume(|buf| {
                if false {
                    let f = EthernetFrame::new_unchecked(&mut *buf);
                    let pp = PrettyPrinter::<EthernetFrame<&mut [u8]>>::print(&f);
                    eprintln!("client thread got {}", pp);
                }
                let tx = TxBuffer::from_packet(tx_po.clone(), buf.len(), packet, false);
                device.transmit(tx);
                //if let Some(dtx) = device.transmit(Instant::now()) {
                //    dtx.consume(buf.len(), |dbuf| dbuf.copy_from_slice(buf));
                //  }
            })
        }

        let rx_waiter = ep.rx_waiter();
        if ep.has_pending_msg_from_client() {
            continue;
        }
        drop(ep);

        let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(rx_waiter)], None);
    }
}
