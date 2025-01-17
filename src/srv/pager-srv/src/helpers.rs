use std::{io::Error, sync::Arc};

use miette::{IntoDiagnostic, Result};
use twizzler_abi::pager::{CompletionToPager, ObjectRange, PhysRange, RequestFromPager};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::physrw;

/// A constant representing the page size (4096 bytes per page).
pub const PAGE: u64 = 4096;

/// Converts a `PhysRange` into the number of pages (4096 bytes per page).
/// Returns a `u64` representing the total number of pages in the range.
pub fn physrange_to_pages(phys_range: &PhysRange) -> u64 {
    if phys_range.end <= phys_range.start {
        return 0;
    }
    let range_size = phys_range.end - phys_range.start;
    (range_size + PAGE - 1) / PAGE // Add PAGE - 1 for ceiling division by PAGE
}

/// Converts a `PhysRange` into the number of pages (4096 bytes per page).
/// Returns a `u64` representing the total number of pages in the range.
pub fn page_to_physrange(page_num: usize, range_start: u64) -> PhysRange {
    let start = ((page_num as u64) * PAGE) + range_start;
    let end = start + PAGE;

    return PhysRange { start, end };
}

/// Converts an `ObjectRange` representing a single page into the page number.
/// Assumes the range is within a valid memory mapping and spans exactly one page (4096 bytes).
/// Returns the page number starting at 0.
pub fn objectrange_to_page_number(object_range: &ObjectRange) -> Option<u64> {
    if object_range.end - object_range.start != PAGE {
        return None; // Invalid ObjectRange for a single page
    }
    Some(object_range.start / PAGE)
}

pub async fn page_in(
    rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    obj_id: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    meta: bool,
) -> Result<()> {
    assert_eq!(obj_range.len(), 0x1000);
    assert_eq!(phys_range.len(), 0x1000);

    let mut buf = [0; 0x1000];
    let start = if meta {
        obj_range.start + (1024 * 1024 * 1024)
    } else {
        obj_range.start
    };
    tracing::trace!("read_exact: offset: {}", start);
    let res = object_store::read_exact(obj_id.raw(), &mut buf, start)
        .inspect_err(|e| tracing::trace!("error in read from object store: {}", e));
    if res.is_err() {
        buf.fill(0);
    }

    physrw::fill_physical_pages(rq, &buf, phys_range).await
}

pub async fn page_out(
    rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    obj_id: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    meta: bool,
) -> Result<()> {
    assert_eq!(obj_range.len(), 0x1000);
    assert_eq!(phys_range.len(), 0x1000);

    tracing::trace!("pageout: {}: {:?} {:?}", obj_id, obj_range, phys_range);
    let mut buf = [0; 0x1000];
    physrw::read_physical_pages(rq, &mut buf, phys_range).await?;
    let start = if meta {
        obj_range.start + (1024 * 1024 * 1024)
    } else {
        obj_range.start
    };
    tracing::trace!("write_all: offset: {}", start);
    object_store::write_all(obj_id.raw(), &buf, start)
        .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))
        .into_diagnostic()
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
        assert_eq!(objectrange_to_page_number(&range), Some(0));

        let range = ObjectRange {
            start: 4096,
            end: 8192,
        };
        assert_eq!(objectrange_to_page_number(&range), Some(1));

        let range = ObjectRange {
            start: 0,
            end: 8192,
        }; // Invalid range for one page
        assert_eq!(objectrange_to_page_number(&range), None);

        let range = ObjectRange {
            start: 8192,
            end: 12288,
        };
        assert_eq!(objectrange_to_page_number(&range), Some(2));
    }
}
