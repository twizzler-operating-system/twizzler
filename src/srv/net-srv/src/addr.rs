use std::{
    collections::{HashSet, VecDeque},
    sync::Mutex,
};

use smoltcp::wire::EthernetAddress;

/// QEMU user networking's default guest address, which `qemu.rs`'s `hostfwd` targets. It is
/// never handed out automatically: only a client that asks for it (sshd, via `TWZ_NET_ADDR` in
/// init) gets it, so the forwarded port lands on sshd whatever order compartments open in. `.2`
/// (gateway) and `.3` (DNS) belong to slirp.
pub const HOSTFWD_OCTET: u8 = 15;
const FIRST_AUTO_OCTET: u8 = 16;
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

/// FIFO on purpose: a released octet goes to the back, so it is reused only after every other
/// free one. Its MAC is derived from it, so neighbor caches stay right across reuse; the quiet
/// period is for the peer side, where slirp or a remote may still hold TCP state for the old
/// holder's tuples. Same reasoning as `PortAssigner::get_ephemeral_port`.
pub struct AddrAssigner {
    inner: Mutex<Pool>,
}

struct Pool {
    free: VecDeque<u8>,
    taken: HashSet<u8>,
}

impl AddrAssigner {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Pool {
                free: (FIRST_AUTO_OCTET..=LAST_OCTET).collect(),
                taken: HashSet::new(),
            }),
        }
    }

    pub fn allocate(&self) -> Option<ClientAddr> {
        let mut pool = self.inner.lock().unwrap();
        let octet = pool.free.pop_front()?;
        pool.taken.insert(octet);
        Some(ClientAddr { octet })
    }

    /// A specific octet, if nobody holds it. Pulls it out of the free list if it is there.
    pub fn reserve(&self, octet: u8) -> Option<ClientAddr> {
        if !(HOSTFWD_OCTET..=LAST_OCTET).contains(&octet) {
            return None;
        }
        let mut pool = self.inner.lock().unwrap();
        if !pool.taken.insert(octet) {
            return None;
        }
        pool.free.retain(|o| *o != octet);
        Some(ClientAddr { octet })
    }

    pub fn release(&self, addr: ClientAddr) {
        let mut pool = self.inner.lock().unwrap();
        pool.taken.remove(&addr.octet);
        if addr.octet != HOSTFWD_OCTET {
            pool.free.push_back(addr.octet);
        }
    }
}
