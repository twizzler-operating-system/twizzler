#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct Ipv4Addr {
    addr: u32,
}

impl Ipv4Addr {
    fn is_localhost(&self) -> bool {
        addr == 0x7F000001
    }

    fn localhost() -> Self {
        Self { addr: 0x7F000001 }
    }
}
