use twizzler_net::{ConnectionId, PacketData, TxCompletion, TxCompletionError};

use crate::HandleRef;

pub async fn send_packet(
    handle: &HandleRef,
    conn_id: ConnectionId,
    packet_data: PacketData,
) -> TxCompletion {
    let info = match handle.data().get_endpoint_info(conn_id) {
        Some(info) => info,
        None => return TxCompletion::Error(TxCompletionError::NoSuchConnection),
    };

    match info.dest_address().1 {
        twizzler_net::addr::ServiceAddr::Null => {
            crate::network::send_raw_packet(handle, info, packet_data).await
        }
        _ => crate::transport::send_packet(handle, info, packet_data).await,
    }
}
