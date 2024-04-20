use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use twizzler_async::FlagBlock;

use crate::link::{
    ethernet::{EthernetAddr, EthernetError},
    nic::{NetworkInterface, NicBuffer, SendableBuffer},
};

struct LoopbackInner {
    buffer: VecDeque<Arc<NicBuffer>>,
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

    async fn send_ethernet(
        &self,
        header_buffer: NicBuffer,
        buffers: &[SendableBuffer],
    ) -> Result<(), EthernetError> {
        // yeah i know this is really slow
        let mut inner = self.inner.lock().unwrap();
        let slice = header_buffer.as_bytes();
        let total_len = buffers.iter().fold(0usize, |t, b| t + b.as_bytes().len()) + slice.len();
        let mut nb = NicBuffer::allocate(total_len);
        let nb_bytes = nb.as_bytes_mut();
        nb_bytes[0..slice.len()].copy_from_slice(slice);
        let mut off = slice.len();
        for buffer in buffers {
            let slice = buffer.as_bytes();
            nb_bytes[off..(off + slice.len())].copy_from_slice(slice);
            off += slice.len();
        }
        inner.buffer.push_back(Arc::new(nb));
        self.flag.signal_all();
        Ok(())
    }

    async fn recv_ethernet(&self) -> Result<Vec<Arc<NicBuffer>>, EthernetError> {
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
