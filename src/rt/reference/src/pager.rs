use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::DynamicSecGate;
use twizzler_abi::object::ObjID;

struct PagerAPI {
    _handle: &'static CompartmentHandle,
    full_sync_call: DynamicSecGate<'static, (ObjID,), ()>,
}

static PAGER_API: OnceLock<PagerAPI> = OnceLock::new();

fn pager_api() -> &'static PagerAPI {
    PAGER_API.get_or_init(|| {
        let handle = Box::leak(Box::new(
            CompartmentHandle::lookup("pager-srv").expect("failed to open pager compartment"),
        ));
        let full_sync_call = unsafe {
            handle
                .dynamic_gate::<(ObjID,), ()>("full_object_sync")
                .expect("failed to find full object sync gate call")
        };
        PagerAPI {
            _handle: handle,
            full_sync_call,
        }
    })
}

pub fn sync_object(id: ObjID) {
    (pager_api().full_sync_call)(id).unwrap()
}
