use twizzler_net::{PacketData, TxCompletion};

use crate::{endpoint::EndPointKey, HandleRef};

pub mod ipv4;

pub async fn send_raw_packet(
    _handle: &HandleRef,
    _endpoint_info: EndPointKey,
    _packet_data: PacketData,
) -> TxCompletion {
    todo!()
}
