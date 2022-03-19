use std::{collections::BTreeMap, sync::Mutex};

use twizzler_net::{
    addr::{NodeAddr, ServiceAddr},
    ConnectionId, ListenFlags,
};

use crate::HandleRef;

#[allow(dead_code)]
pub struct EndPoint {
    handle: HandleRef,
    conn_id: ConnectionId,
    info: EndPointKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EndPointKey {
    source: NodeAddr,
    dest: NodeAddr,
    flags: ListenFlags,
    source_service: ServiceAddr,
    dest_service: ServiceAddr,
}

impl EndPointKey {
    pub fn source_address(&self) -> (NodeAddr, ServiceAddr) {
        (self.source, self.source_service)
    }

    pub fn dest_address(&self) -> (NodeAddr, ServiceAddr) {
        (self.dest, self.dest_service)
    }

    pub fn flags(&self) -> ListenFlags {
        self.flags
    }

    pub fn new(
        source: NodeAddr,
        dest: NodeAddr,
        flags: ListenFlags,
        source_service: ServiceAddr,
        dest_service: ServiceAddr,
    ) -> Self {
        Self {
            source,
            dest,
            flags,
            source_service,
            dest_service,
        }
    }
}

lazy_static::lazy_static! {
    static ref ENDPOINTS: Mutex<BTreeMap<EndPointKey, BTreeMap<(u64, ConnectionId), EndPoint>>> = Mutex::new(BTreeMap::new());
}

#[allow(dead_code)]
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
        map.insert(
            (handle.id(), conn_id),
            EndPoint {
                handle,
                conn_id,
                info,
            },
        );
    } else {
        let mut map = BTreeMap::new();
        map.insert(
            (handle.id(), conn_id),
            EndPoint {
                handle,
                conn_id,
                info,
            },
        );
        endpoints.insert(info, map);
    }
}
