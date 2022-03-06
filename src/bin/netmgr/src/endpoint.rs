use std::{collections::BTreeMap, sync::Mutex};

use twizzler_net::{
    addr::{NodeAddr, ProtType, ServiceAddr},
    ConnectionFlags, ConnectionId,
};

use crate::HandleRef;

pub struct EndPoint {
    handle: HandleRef,
    conn_id: ConnectionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EndPointKey {
    source: NodeAddr,
    dest: NodeAddr,
    prot: ProtType,
    flags: ConnectionFlags,
    source_service: ServiceAddr,
    dest_service: ServiceAddr,
}

impl EndPointKey {
    pub fn new(
        source: NodeAddr,
        dest: NodeAddr,
        prot: ProtType,
        flags: ConnectionFlags,
        source_service: ServiceAddr,
        dest_service: ServiceAddr,
    ) -> Self {
        Self {
            source,
            dest,
            prot,
            flags,
            source_service,
            dest_service,
        }
    }
}

lazy_static::lazy_static! {
    static ref ENDPOINTS: Mutex<BTreeMap<EndPointKey, BTreeMap<(u64, ConnectionId), EndPoint>>> = Mutex::new(BTreeMap::new());
}

pub fn foreach_endpoint(info: &EndPointKey, f: impl Fn(&HandleRef, ConnectionId)) {
    let endpoints = ENDPOINTS.lock().unwrap();
    if let Some(map) = endpoints.get(&info) {
        for item in map {
            f(&item.1.handle, item.1.conn_id);
        }
    }
}

pub fn add_endpoint(info: EndPointKey, handle: HandleRef, conn_id: ConnectionId) {
    let mut endpoints = ENDPOINTS.lock().unwrap();
    if let Some(map) = endpoints.get_mut(&info) {
        map.insert((handle.id(), conn_id), EndPoint { handle, conn_id });
    } else {
        let mut map = BTreeMap::new();
        map.insert((handle.id(), conn_id), EndPoint { handle, conn_id });
        endpoints.insert(info, map);
    }
}
