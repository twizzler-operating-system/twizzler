use twizzler_net::{
    buffer::ManagedBuffer, ConnectionId, PacketData, TxCompletion, TxCompletionError, TxRequest,
};

use crate::{icmp, HandleRef};

pub async fn send_packet(
    handle: &HandleRef,
    conn_id: ConnectionId,
    packet_data: PacketData,
) -> TxCompletion {
    let info = match handle.data().get_endpoint_info(conn_id) {
        Some(info) => info,
        None => return TxCompletion::Error(TxCompletionError::NoSuchConnection),
    };

    //let dest_addr = info.dest_address();
    match info.protocol_type() {
        twizzler_net::addr::ProtType::Raw => todo!(),
        twizzler_net::addr::ProtType::Icmp => {
            return icmp::send_packet(handle, info, packet_data).await
        }
        twizzler_net::addr::ProtType::Tcp => todo!(),
        twizzler_net::addr::ProtType::Udp => todo!(),
    }

    //TxCompletion::Nothing
}
