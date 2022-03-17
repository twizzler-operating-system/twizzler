use std::collections::BTreeMap;

use twizzler_net::{addr::ServiceAddr, ListenFlags, PacketData, TxCompletion, TxCompletionError};

use crate::{endpoint::EndPointKey, link::IncomingPacketInfo, HandleRef};

pub mod icmp;
pub mod tcp;
pub mod udp;

enum RawSupport {
    NoRaw,
    RawAllowed,
    OnlyRaw,
}

#[async_trait::async_trait]
trait TransportProto: Sync + Send {
    async fn send_packet(
        &self,
        handle: &HandleRef,
        endpoint_info: EndPointKey,
        packet_data: PacketData,
    ) -> TxCompletion;

    async fn handle_packet(&self, info: IncomingPacketInfo);

    fn raw_support(&self) -> RawSupport;
}

lazy_static::lazy_static! {
    static ref PROTOS: BTreeMap<ServiceAddr, Box<dyn TransportProto>> = {
        let mut map: BTreeMap<ServiceAddr, Box<dyn TransportProto>> = BTreeMap::new();
       // let (key, value) = tcp::init();
      //  map.insert(key, value);
       // let (key, value) = udp::init();
      //  map.insert(key, value);
        let (key, value) = icmp::init();
        map.insert(key, Box::new(value));
        map
    };
}

pub async fn send_packet(
    handle: &HandleRef,
    endpoint_info: EndPointKey,
    packet_data: PacketData,
) -> TxCompletion {
    let dest_service_any = endpoint_info.dest_address().1.any();
    if let Some(proto) = PROTOS.get(&dest_service_any) {
        proto.send_packet(handle, endpoint_info, packet_data).await
    } else {
        TxCompletion::Error(TxCompletionError::InvalidArgument)
    }
}

pub async fn handle_packet(addr: ServiceAddr, info: IncomingPacketInfo) {
    if let Some(proto) = PROTOS.get(&addr) {
        proto.handle_packet(info);
    }
}
