use std::{collections::VecDeque, sync::Mutex};

use twizzler_async::FlagBlock;
use twizzler_net::buffer::ManagedBuffer;

use crate::{
    ethernet::{EthernetAddr, EthernetError},
    nic::{NetworkInterface, NicBuffer},
};

struct LoopbackInner {
    buffer: VecDeque<NicBuffer>,
}

pub(super) struct Loopback {
    inner: Mutex<LoopbackInner>,
    flag: FlagBlock,
}

impl Loopback {
    pub(super) fn new() -> Self {
        Self {
            inner: Mutex::new(LoopbackInner {
                buffer: VecDeque::new(),
            }),
            flag: FlagBlock::new(),
        }
    }
}

#[async_trait::async_trait]
impl NetworkInterface for Loopback {
    fn get_ethernet_addr(&self) -> EthernetAddr {
        EthernetAddr::from([0; 6])
    }

    async fn send_ethernet(&self, buffers: &[ManagedBuffer]) -> Result<(), EthernetError> {
        // yeah i know this is really slow
        let mut inner = self.inner.lock().unwrap();
        for buffer in buffers {
            let slice = buffer.as_bytes();
            let mut nb = NicBuffer::allocate(slice.len());
            nb.as_bytes_mut().copy_from_slice(slice);
            inner.buffer.push_back(nb);
        }
        self.flag.signal_all();
        Ok(())
    }

    async fn recv_ethernet(&self) -> Result<Vec<NicBuffer>, EthernetError> {
        loop {
            let fut = {
                let mut inner = self.inner.lock().unwrap();
                if !inner.buffer.is_empty() {
                    let mut v = vec![];
                    while let Some(buf) = inner.buffer.pop_front() {
                        v.push(buf);
                    }
                    return Ok(v);
                }

                self.flag.wait()
            };
            fut.await;
        }
    }
}
