use twizzler_net::{
    addr::{Ipv4Addr, NodeAddr},
    ListenInfo, TxCompletion,
};

use crate::{endpoint, HandleRef};

pub fn setup_listen(handle: &HandleRef, conn_info: ListenInfo) -> TxCompletion {
    let conn_id = handle.data().new_conn_id();
    let address = conn_info.address();
    // TODO: get our address?
    let our_address = NodeAddr::Ipv4(Ipv4Addr::localhost());
    let key = endpoint::EndPointKey::new(
        address.0,
        our_address,
        conn_info.flags(),
        address.1,
        address.1,
    );
    handle.data().add_endpoint_info(conn_id, key);
    endpoint::add_endpoint(key, handle.clone(), conn_id);
    TxCompletion::ListenReady(conn_id)
}
