use std::sync::Mutex;

use smoltcp::wire::EthernetAddress;

/// QEMU user networking hands the guest 10.0.2.15 first, and `qemu.rs`'s `hostfwd` targets that
/// address, so the first client to open keeps it and later ones climb from there. `.2` (gateway)
/// and `.3` (DNS) belong to slirp.
const FIRST_OCTET: u8 = 15;
const LAST_OCTET: u8 = 250;

/// Per-client L2/L3 identity.
///
/// Every client used to be handed the same address and the same MAC, which meant `device_thread`'s
/// broadcast delivered every inbound frame to a stack that had no socket for it -- and smoltcp
/// answers an unmatched TCP segment with an RST (`process_tcp`). Two networked compartments
/// therefore tore down each other's connections. Distinct MACs fix that at L2 for free:
/// `process_ethernet` drops a frame whose destination is neither broadcast/multicast nor the
/// interface's own address, before TCP ever sees it. Distinct addresses are what let two
/// compartments name each other at all.
#[derive(Clone, Copy, Debug)]
pub struct ClientAddr {
    octet: u8,
}

impl ClientAddr {
    pub fn ipv4(&self) -> [u8; 4] {
        [10, 0, 2, self.octet]
    }

    /// Locally-administered unicast (the `0x02` bit), keyed by the host octet so the mapping
    /// between a client's MAC and its address is readable in a packet dump.
    pub fn hwaddr(&self) -> EthernetAddress {
        EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, self.octet])
    }
}

pub struct AddrAssigner {
    inner: Mutex<Vec<u8>>,
}

impl AddrAssigner {
    pub fn new() -> Self {
        // Popped from the end, so the first client gets FIRST_OCTET.
        Self {
            inner: Mutex::new((FIRST_OCTET..=LAST_OCTET).rev().collect()),
        }
    }

    pub fn allocate(&self) -> Option<ClientAddr> {
        self.inner
            .lock()
            .unwrap()
            .pop()
            .map(|octet| ClientAddr { octet })
    }

    pub fn release(&self, addr: ClientAddr) {
        self.inner.lock().unwrap().push(addr.octet);
    }
}
