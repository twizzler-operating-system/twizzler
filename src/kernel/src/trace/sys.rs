use twizzler_abi::{object::ObjID, syscall::TraceSpec};
use twizzler_rt_abi::error::TwzError;

use super::mgr::TRACE_MGR;

pub fn sys_ktrace(target: ObjID, spec: Option<&TraceSpec>) -> Result<u64, TwzError> {
    log::info!("sys_ktrace: {}: {:?}", target, spec);
    if let Some(spec) = spec {
        TRACE_MGR.add_sink(target, *spec)?;
    } else {
        TRACE_MGR.remove_sink(target)?;
    }
    Ok(0)
}
