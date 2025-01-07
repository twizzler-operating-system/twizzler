use std::sync::{Arc, OnceLock};

use twizzler_abi::pager::{CompletionToPager, PagerRequest, PhysRange, RequestFromPager};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::send_request;

type Queue = QueueSender<RequestFromPager, CompletionToPager>;
type QueueRef = Arc<Queue>;

static SCTX_ID: OnceLock<ObjID> = OnceLock::new();

fn get_sctx_id() -> ObjID {
    *SCTX_ID.get_or_init(|| twizzler_abi::syscall::sys_thread_active_sctx_id())
}

fn get_object(ptr: *const u8) -> (ObjID, usize) {
    todo!()
}

async fn do_physrw_request(
    queue: &QueueRef,
    target_object: ObjID,
    offset: usize,
    len: usize,
    phys: PhysRange,
    write_phys: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = RequestFromPager::new(PagerRequest::CopyUserPhys {
        target_object,
        offset,
        len,
        phys,
        write_phys,
    });
    let comp = send_request(queue, request).await?;
    match comp.data() {
        twizzler_abi::pager::PagerCompletionData::Okay => Ok(()),
        _ => Err("invalid pager completion".to_owned().into()),
    }
}

/// Writes buf.len() bytes from the buffer into physical addresses specified in phys. If the
/// supplied physical range is shorter than the buffer, then the remaining bytes in the buffer are
/// filled with 0.
pub async fn fill_physical_pages(
    queue: &QueueRef,
    buf: &[u8],
    phys: PhysRange,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, true).await
}

/// Reads buf.len() bytes from physical addresses in phys into the buffer. If the supplied physical
/// range is shorter than the buffer, then the remaining bytes in the buffer are filled with 0.
pub async fn read_physical_pages(
    queue: &QueueRef,
    buf: &mut [u8],
    phys: PhysRange,
) -> Result<(), Box<dyn std::error::Error>> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, false).await
}
