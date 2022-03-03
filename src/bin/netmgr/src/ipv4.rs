use std::sync::Arc;

use twizzler_net::{addr::Ipv4Addr, buffer::ManagedBuffer, NmHandleManager};

use crate::ethernet::EthernetAddr;

pub async fn send_to(
    handle: &Arc<NmHandleManager>,
    addr: Ipv4Addr,
    buffer: ManagedBuffer<'_>,
) -> Result<(), Ipv4SendError> {
    if addr.is_localhost() {
        let lo = crate::nics::lookup_nic(&EthernetAddr::local()).ok_or(Ipv4Error::Unknown)?;
        let buffer = build_ipv4_header();
        return lo
            .send_ethernet(&[buffer])
            .await
            .map_err(Ipv4Error::Unknown);
    }
    todo!()
}

pub enum Ipv4SendError {
    Unknown,
}
