use std::sync::Arc;

use miette::{IntoDiagnostic, Result};
use object_store::{PageRequest, PagingImp};
use twizzler_abi::pager::{CompletionToPager, ObjectRange, PhysRange, RequestFromPager};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::{disk::DiskPageRequest, physrw, PagerContext};

/// A constant representing the page size (4096 bytes per page).
pub const PAGE: u64 = 4096;

/// Converts an `ObjectRange` representing a single page into the page number.
/// Assumes the range is within a valid memory mapping and spans exactly one page (4096 bytes).
/// Returns the page number starting at 0.
pub fn _objectrange_to_page_number(object_range: &ObjectRange) -> Option<u64> {
    if object_range.end - object_range.start != PAGE {
        return None; // Invalid ObjectRange for a single page
    }
    Some(object_range.start / PAGE)
}

pub async fn page_in(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    meta: bool,
) -> Result<()> {
    assert_eq!(obj_range.len(), 0x1000);
    assert_eq!(phys_range.len(), 0x1000);

    /*
    let mut buf = [0; 0x1000];
    let res = ctx
        .paged_ostore
        .read_object(obj_id.raw(), start, &mut buf)
        .inspect_err(|e| tracing::debug!("error in read from object store: {}", e));
    if res.is_err() {
        buf.fill(0);
    }
    physrw::fill_physical_pages(&ctx.sender, &buf, phys_range).await
    */
    let imp = ctx
        .disk
        .new_paging_request::<DiskPageRequest>([phys_range.start]);
    let start_page = obj_range.start / DiskPageRequest::page_size() as u64;
    let nr_pages = obj_range.len() / DiskPageRequest::page_size();
    let reqs = vec![PageRequest::new(imp, start_page as i64, nr_pages as u32)];
    page_in_many(ctx, obj_id, reqs).await.map(|_| ())
}

pub async fn page_out(
    ctx: &PagerContext,
    obj_id: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    meta: bool,
) -> Result<()> {
    assert_eq!(obj_range.len(), 0x1000);
    assert_eq!(phys_range.len(), 0x1000);

    tracing::debug!("pageout: {}: {:?} {:?}", obj_id, obj_range, phys_range);
    let mut buf = [0; 0x1000];
    physrw::read_physical_pages(&ctx.sender, &mut buf, phys_range).await?;
    let start = if meta {
        obj_range.start + (1024 * 1024 * 1024)
    } else {
        obj_range.start
    };
    ctx.paged_ostore
        .write_object(obj_id.raw(), start, &buf)
        .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))
        .into_diagnostic()
}

pub async fn page_out_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    reqs: &'static [PageRequest<DiskPageRequest>],
) -> Result<usize> {
    blocking::unblock(move || {
        ctx.paged_ostore
            .page_out_object(obj_id.raw(), reqs)
            .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))
            .into_diagnostic()
    })
    .await
}

pub async fn page_in_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    mut reqs: Vec<PageRequest<DiskPageRequest>>,
) -> Result<usize> {
    blocking::unblock(move || {
        ctx.paged_ostore
            .page_in_object(obj_id.raw(), &mut reqs)
            .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))
            .into_diagnostic()
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_physrange_to_pages() {
        let range = PhysRange {
            start: 0,
            end: 8192,
        };
        assert_eq!(physrange_to_pages(&range), 2);

        let range = PhysRange {
            start: 0,
            end: 4095,
        };
        assert_eq!(physrange_to_pages(&range), 1);

        let range = PhysRange { start: 0, end: 0 };
        assert_eq!(physrange_to_pages(&range), 0);

        let range = PhysRange {
            start: 4096,
            end: 8192,
        };
        assert_eq!(physrange_to_pages(&range), 2);
    }

    #[test]
    fn test_objectrange_to_page_number() {
        let range = ObjectRange {
            start: 0,
            end: 4096,
        };
        assert_eq!(_objectrange_to_page_number(&range), Some(0));

        let range = ObjectRange {
            start: 4096,
            end: 8192,
        };
        assert_eq!(_objectrange_to_page_number(&range), Some(1));

        let range = ObjectRange {
            start: 0,
            end: 8192,
        }; // Invalid range for one page
        assert_eq!(_objectrange_to_page_number(&range), None);

        let range = ObjectRange {
            start: 8192,
            end: 12288,
        };
        assert_eq!(_objectrange_to_page_number(&range), Some(2));
    }
}
