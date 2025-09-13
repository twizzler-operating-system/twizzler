use std::ops::Add;

use object_store::{objid_to_ino, PageRequest, PagedObjectStore};
use twizzler::object::{MetaExt, MetaFlags, MetaInfo, ObjID, MEXT_SIZED};
use twizzler_abi::{
    object::{Protections, MAX_SIZE},
    pager::{ObjectRange, PhysRange},
};
use twizzler_rt_abi::{object::Nonce, Result};

use crate::PagerContext;

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

//https://stackoverflow.com/questions/50380352/how-can-i-group-consecutive-integers-in-a-vector-in-rust
pub fn consecutive_slices<T: PartialEq + Add<u64> + Copy>(data: &[T]) -> impl Iterator<Item = &[T]>
where
    T::Output: PartialEq<T>,
{
    let mut slice_start = 0;
    (1..=data.len()).flat_map(move |i| {
        if i == data.len() || data[i - 1] + 1u64 != data[i] {
            let begin = slice_start;
            slice_start = i;
            Some(&data[begin..i])
        } else {
            None
        }
    })
}

pub async fn page_in(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
) -> Result<()> {
    assert_eq!(obj_range.len(), PAGE as usize);
    assert_eq!(phys_range.len(), PAGE as usize);

    let mut start_page = obj_range.start / PAGE;

    if obj_range.start == (MAX_SIZE as u64) - PAGE {
        tracing::debug!("found meta page, using 0 page");
        start_page = 0;
        if objid_to_ino(obj_id.raw()).is_some() {
            unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
                ::core::slice::from_raw_parts(
                    (p as *const T) as *const u8,
                    ::core::mem::size_of::<T>(),
                )
            }

            let len = ctx
                .paged_ostore(None)?
                .find_external(obj_id.raw())
                .await
                .inspect_err(|e| tracing::warn!("failed to find extern inode: {}", e))?;
            tracing::debug!("building meta page for external file, len: {}", len);
            let mut buffer = [0; PAGE as usize];
            let meta = MetaInfo {
                nonce: Nonce(0),
                kuid: ObjID::new(0),
                flags: MetaFlags::empty(),
                default_prot: Protections::all(),
                fotcount: 0,
                extcount: 1,
            };
            let me = MetaExt {
                tag: MEXT_SIZED,
                value: len as u64,
            };
            unsafe {
                buffer[0..size_of::<MetaInfo>()].copy_from_slice(any_as_u8_slice(&meta));
                buffer[size_of::<MetaInfo>()..(size_of::<MetaInfo>() + size_of::<MetaExt>())]
                    .copy_from_slice(any_as_u8_slice(&me));
            }
            crate::physrw::fill_physical_pages(&buffer, phys_range).await?;
            return Ok(());
        }
    }

    let nr_pages = obj_range.len() / PAGE as usize;
    let mut reqs = [PageRequest::new(start_page as i64, nr_pages as u32)];
    page_in_many(ctx, obj_id, &mut reqs).await.map(|_| ())
}

pub async fn page_out_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    reqs: &mut [PageRequest],
) -> Result<usize> {
    let mut reqslice = &mut reqs[..];
    while reqslice.len() > 0 {
        let donecount = ctx
            .paged_ostore(None)?
            .page_out_object(obj_id.raw(), reqslice)
            .await
            .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))?;
        reqslice = &mut reqslice[donecount..];
    }
    Ok(reqs.len())
}

pub async fn page_in_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    reqs: &mut [PageRequest],
) -> Result<usize> {
    let ret = ctx
        .paged_ostore(None)?
        .page_in_object(obj_id.raw(), reqs)
        .await
        .inspect_err(|e| tracing::warn!("error in read from object store: {}", e))?;
    Ok(ret)
}

#[cfg(test)]
mod tests {
    use super::*;

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
