use twizzler_net::{PacketData, TxCompletion};

use crate::{endpoint::EndPointKey, HandleRef};

pub mod ipv4;

pub async fn send_raw_packet(
    handle: &HandleRef,
    endpoint_info: EndPointKey,
    packet_data: PacketData,
) -> TxCompletion {
    todo!()
}
