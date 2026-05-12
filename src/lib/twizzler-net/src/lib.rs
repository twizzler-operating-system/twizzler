use secgate::TwzError;
pub use twizzler_io::packet;

mod client;
pub mod drivers;
mod endpoint;
mod server;

pub use client::{
    NetClient, NetClientConfig, NetClientOpenInfo, NetClientRxToken, NetClientTxToken,
    net_alloc_port, net_release_port,
};
pub use server::{NetServer, NetServerRxToken, NetServerTxToken};

pub type PacketNum = u32;

pub const MAX_PACKETS_SET: usize = 8;
pub const INVALID_PACKET: PacketNum = !0;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PacketSet([u32; MAX_PACKETS_SET]);

impl PacketSet {
    pub fn new() -> Self {
        Self([INVALID_PACKET; _])
    }

    pub fn from_slice(slice: &[u32]) -> (Self, usize) {
        let mut arr = [INVALID_PACKET; _];
        let len = MAX_PACKETS_SET.min(slice.len());
        arr[0..len].copy_from_slice(&slice[0..len]);
        (Self(arr), len)
    }

    pub fn push(&mut self, num: PacketNum) -> Option<()> {
        let inv = self.0.iter().position(|p| *p == INVALID_PACKET)?;
        self.0[inv] = num;
        Some(())
    }
}

pub struct PacketSetIter<'a> {
    set: &'a PacketSet,
    index: usize,
}

impl<'a> Iterator for PacketSetIter<'a> {
    type Item = PacketNum;

    fn next(&mut self) -> Option<Self::Item> {
        let mut num = INVALID_PACKET;
        while num == INVALID_PACKET && self.index < MAX_PACKETS_SET {
            num = self.set.0[self.index];
            self.index += 1;
        }
        if num == INVALID_PACKET {
            None
        } else {
            Some(num)
        }
    }
}

impl<'a> IntoIterator for &'a PacketSet {
    type Item = PacketNum;
    type IntoIter = PacketSetIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        PacketSetIter {
            set: self,
            index: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ServerMsg {
    kind: ServerMsgKind,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ClientMsgKind {
    Tx(PacketSet),
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub enum ServerMsgKind {
    Tx(PacketSet),
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClientMsg {
    kind: ClientMsgKind,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ClientRet {}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ServerRet {}

#[secgate::gatecall]
pub fn start_network() -> Result<(), TwzError> {}

#[secgate::gatecall]
fn twz_net_drop_client(handle: secgate::util::Descriptor) -> Result<(), TwzError> {}

#[secgate::gatecall]
fn twz_net_open_client(config: NetClientConfig) -> Result<NetClientOpenInfo, TwzError> {}

#[secgate::gatecall]
fn twz_net_alloc_port(
    handle: secgate::util::Descriptor,
    port: Option<u16>,
) -> Result<u16, TwzError> {
}

#[secgate::gatecall]
fn twz_net_release_port(handle: secgate::util::Descriptor, port: u16) -> Result<(), TwzError> {}
