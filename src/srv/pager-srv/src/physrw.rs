use std::sync::Arc;

use twizzler::object::ObjID;
use twizzler_abi::pager::{CompletionToPager, PagerRequest, PhysRange, RequestFromPager};
use twizzler_queue::QueueSender;
use twizzler_rt_abi::{error::TwzError, Result};

type Queue = QueueSender<RequestFromPager, CompletionToPager>;
type QueueRef = Arc<Queue>;

fn get_object(ptr: *const u8) -> (ObjID, usize) {
    let handle = twizzler_rt_abi::object::twz_rt_get_object_handle(ptr).unwrap();
    (handle.id(), unsafe {
        ptr.offset_from_unsigned(handle.start())
    })
}

pub async fn register_phys(queue: &QueueRef, start: u64, len: u64) -> Result<()> {
    let request = RequestFromPager::new(PagerRequest::RegisterPhys(start, len));
    let comp = queue.submit_and_wait(request).await?;
    match comp.data() {
        twizzler_abi::pager::PagerCompletionData::Okay => Ok(()),
        twizzler_abi::pager::PagerCompletionData::Error(e) => Err(e.error()),
        _ => Err(TwzError::INVALID_ARGUMENT),
    }
}

async fn do_physrw_request(
    queue: &QueueRef,
    target_object: ObjID,
    offset: usize,
    len: usize,
    phys: PhysRange,
    write_phys: bool,
) -> Result<()> {
    let request = RequestFromPager::new(PagerRequest::CopyUserPhys {
        target_object,
        offset,
        len,
        phys,
        write_phys,
    });
    let comp = queue.submit_and_wait(request).await?;
    match comp.data() {
        twizzler_abi::pager::PagerCompletionData::Okay => Ok(()),
        twizzler_abi::pager::PagerCompletionData::Error(e) => Err(e.error()),
        _ => Err(TwzError::INVALID_ARGUMENT),
    }
}

/// Writes phys.len() bytes from the buffer into physical addresses specified in phys. If the
/// supplied buffer is shorter than the physical range, then the remaining bytes in the physical
/// memory are filled with 0.
pub async fn fill_physical_pages(queue: &QueueRef, buf: &[u8], phys: PhysRange) -> Result<()> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, true).await
}

/// Reads buf.len() bytes from physical addresses in phys into the buffer. If the supplied physical
/// range is shorter than the buffer, then the remaining bytes in the buffer are filled with 0.
#[allow(dead_code)]
pub async fn read_physical_pages(queue: &QueueRef, buf: &mut [u8], phys: PhysRange) -> Result<()> {
    let obj = get_object(buf.as_ptr());
    do_physrw_request(queue, obj.0, obj.1, buf.len(), phys, false).await
}
