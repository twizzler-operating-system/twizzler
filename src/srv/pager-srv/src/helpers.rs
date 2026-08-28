use object_store::{objid_to_ino, PageRequest, PagedObjectStore, ProbeMiss};
use twizzler::object::{MetaExt, MetaFlags, MetaInfo, ObjID, MEXT_MTIME, MEXT_SIZED};
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

/// Group `data` into maximal runs, where `is_adjacent(a, b)` reports whether `b` immediately
/// follows `a`. Taking the predicate explicitly avoids encoding adjacency in an `Add` impl,
/// which could only express "exactly one page later".
//https://stackoverflow.com/questions/50380352/how-can-i-group-consecutive-integers-in-a-vector-in-rust
pub fn consecutive_slices<T>(
    data: &[T],
    mut is_adjacent: impl FnMut(&T, &T) -> bool,
) -> impl Iterator<Item = &[T]> {
    let mut slice_start = 0;
    (1..=data.len()).flat_map(move |i| {
        if i == data.len() || !is_adjacent(&data[i - 1], &data[i]) {
            let begin = slice_start;
            slice_start = i;
            Some(&data[begin..i])
        } else {
            None
        }
    })
}

/// The meta page an external (ino-backed) file gets.
///
/// These objects have no stored metadata -- there is a POSIX file underneath, not a Twizzler
/// object -- so the pager makes one up from the file's length. `lookup_object` reports the same
/// values to the kernel as [ObjectInfo] fields, hence one definition rather than two.
pub const EXTERNAL_META: MetaInfo = MetaInfo {
    nonce: Nonce(0),
    kuid: ObjID::new(0),
    flags: MetaFlags::empty(),
    default_prot: Protections::all(),
    fotcount: 0,
    extcount: 2,
};

/// Write [EXTERNAL_META] plus its `MEXT_SIZED` (file length) and `MEXT_MTIME` (store mtime,
/// seconds; 0 when the backend keeps none) extensions into a page-sized buffer.
fn fill_external_meta(buffer: &mut [u8; PAGE as usize], len: u64, mtime: u32) {
    unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
        ::core::slice::from_raw_parts((p as *const T) as *const u8, ::core::mem::size_of::<T>())
    }
    let me = MetaExt::new(MEXT_SIZED, len);
    let mt = MetaExt::new(MEXT_MTIME, mtime as u64);
    unsafe {
        buffer[0..size_of::<MetaInfo>()].copy_from_slice(any_as_u8_slice(&EXTERNAL_META));
        buffer[size_of::<MetaInfo>()..(size_of::<MetaInfo>() + size_of::<MetaExt>())]
            .copy_from_slice(any_as_u8_slice(&me));
        buffer[(size_of::<MetaInfo>() + size_of::<MetaExt>())
            ..(size_of::<MetaInfo>() + 2 * size_of::<MetaExt>())]
            .copy_from_slice(any_as_u8_slice(&mt));
    }
}

/// Fill a fresh physical page with an external file's meta page.
pub fn page_in_external_meta(ctx: &'static PagerContext, obj_id: ObjID) -> Result<PhysRange> {
    let len = ctx
        .paged_ostore(None)?
        .len(obj_id.raw())
        .inspect_err(|e| tracing::warn!("failed to find extern inode: {}", e))?;
    let mtime = ctx.paged_ostore(None)?.mtime(obj_id.raw()).unwrap_or(0);
    let phys_range = {
        let page = match ctx.data.try_alloc_page() {
            Ok(page) => page,
            Err(mw) => {
                tracing::warn!("out of memory -- task waiting");
                mw.wait()
            }
        };
        PhysRange::new(page, page + PAGE)
    };
    tracing::debug!("building meta page for external file, len: {}", len);
    let mut buffer = [0; PAGE as usize];
    fill_external_meta(&mut buffer, len, mtime);
    crate::physrw::fill_physical_pages(&buffer, phys_range)?;
    Ok(phys_range)
}

/// Whether paging in this range would have to take the object store's fs lock -- which is held
/// across disk I/O, so a lane that takes it can park for a whole transfer behind a lane of any
/// other priority class.
///
/// Mirrors [page_in]'s range-to-store-page mapping, including the meta page: for an external
/// (ext4-backed) object it is synthesized from the length alone, so a cached length is the whole
/// question there. Answers "no" before the store is open, which is the pre-store behavior.
pub fn page_in_would_block(ctx: &PagerContext, obj_id: ObjID, obj_range: ObjectRange) -> ProbeMiss {
    let Some(store) = ctx.try_paged_ostore() else {
        return ProbeMiss::Cached;
    };
    let mut start_page = obj_range.start / PAGE;
    if obj_range.start == (MAX_SIZE as u64) - PAGE {
        if objid_to_ino(obj_id.raw()).is_some() {
            // An external file's meta page is synthesized from the length alone, so a cached
            // length is the whole answer and extents never come into it.
            return if store.len_is_cached(obj_id.raw()) {
                ProbeMiss::Cached
            } else {
                ProbeMiss::Len
            };
        }
        start_page = 0;
    }
    store.page_in_would_block(obj_id.raw(), start_page, obj_range.page_count() as u32)
}

pub fn page_in(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    obj_range: ObjectRange,
) -> Result<PhysRange> {
    assert_eq!(obj_range.len(), PAGE as usize);

    let mut start_page = obj_range.start / PAGE;

    if obj_range.start == (MAX_SIZE as u64) - PAGE {
        tracing::debug!("found meta page, using 0 page",);
        start_page = 0;
        if objid_to_ino(obj_id.raw()).is_some() {
            return page_in_external_meta(ctx, obj_id);
        }
    }

    let nr_pages = obj_range.len() / PAGE as usize;
    let mut reqs = [PageRequest::new(start_page as i64, nr_pages as u32)];
    page_in_many(ctx, obj_id, &mut reqs).map(|_| ())?;
    let range = reqs.first().unwrap().phys_list.first().unwrap().range;
    Ok(range)
}

pub fn page_out_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    reqs: &mut [PageRequest],
) -> Result<usize> {
    let mut reqslice = &mut reqs[..];
    while reqslice.len() > 0 {
        let donecount = ctx
            .paged_ostore(None)?
            .page_out_object(obj_id.raw(), reqslice)
            .inspect_err(|e| tracing::warn!("error in write to object store: {}", e))?;
        reqslice = &mut reqslice[donecount..];
    }
    Ok(reqs.len())
}

pub fn page_in_many(
    ctx: &'static PagerContext,
    obj_id: ObjID,
    reqs: &mut [PageRequest],
) -> Result<usize> {
    let ret = ctx
        .paged_ostore(None)?
        .page_in_object(obj_id.raw(), reqs)
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
