use std::fmt::Display;

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub struct Ipv4Addr {
    addr: u32,
}

impl Ipv4Addr {
    pub fn is_localhost(&self) -> bool {
        self.addr == 0x7F000001
    }

    pub fn localhost() -> Self {
        Self { addr: 0x7F000001 }
    }
}

impl From<Ipv4Addr> for u32 {
    fn from(x: Ipv4Addr) -> Self {
        x.addr
    }
}

impl From<u32> for Ipv4Addr {
    fn from(x: u32) -> Self {
        Self { addr: x }
    }
}

impl Display for Ipv4Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            (self.addr >> 24) & 0xff,
            (self.addr >> 16) & 0xff,
            (self.addr >> 8) & 0xff,
            (self.addr) & 0xff,
        )
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum NodeAddr {
    Ipv4(Ipv4Addr),
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum ServiceAddr {
    Null,
    Icmp,
    Tcp(u16),
    Udp(u16),
}

impl ServiceAddr {
    pub fn any(&self) -> Self {
        match self {
            ServiceAddr::Tcp(_) => ServiceAddr::Tcp(0),
            ServiceAddr::Udp(_) => ServiceAddr::Udp(0),
            _ => *self,
        }
    }
}
