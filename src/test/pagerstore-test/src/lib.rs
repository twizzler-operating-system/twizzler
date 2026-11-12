//! A pager object store with a few synthetic, read-only objects, for the tests.

use pager_store::{
    ABI_VERSION, EXTENT_DATA, Extent, PAGE_SIZE as PAGE_SIZE_USIZE, Status, StoreTable,
};

const PAGE_SIZE: u64 = PAGE_SIZE_USIZE as u64;
pub const ID_MASK: u128 = 0xffff_ffff << 96;
pub const ID_VALUE: u128 = 0x5354_4f52 << 96;
pub const OBJECTS: u64 = 2;

/// Object `ID_VALUE | n` for `n` in `1..=OBJECTS`: the null page, then `n * 384` data pages.
pub fn object_len(n: u64) -> u64 {
    (1 + n * 384) * PAGE_SIZE
}

/// The u64 at byte offset `off` (8-aligned) of object `n`.
pub fn word(n: u64, off: u64) -> u64 {
    (n << 56) | off
}

fn object(id: u128) -> Option<u64> {
    let n = (id & !ID_MASK) as u64;
    (id & ID_MASK == ID_VALUE && (1..=OBJECTS).contains(&n)).then_some(n)
}

unsafe extern "C" fn len(id: u128, len: *mut u64) -> Status {
    let Some(n) = object(id) else {
        return Status::NOT_FOUND;
    };
    unsafe { len.write(object_len(n)) };
    Status::OK
}

unsafe extern "C" fn page_in(
    id: u128,
    start_page: u64,
    nr_pages: u32,
    data: *mut u8,
    out: *mut Extent,
    cap: u32,
    count: *mut u32,
) -> Status {
    let Some(n) = object(id) else {
        return Status::NOT_FOUND;
    };
    if cap == 0 || nr_pages == 0 {
        return Status::IO_ERROR;
    }
    let data = unsafe { std::slice::from_raw_parts_mut(data, nr_pages as usize * PAGE_SIZE_USIZE) };
    let base = start_page * PAGE_SIZE;
    for (i, w) in data.chunks_exact_mut(8).enumerate() {
        let off = base + i as u64 * 8;
        let v = if off < object_len(n) { word(n, off) } else { 0 };
        w.copy_from_slice(&v.to_le_bytes());
    }
    unsafe {
        out.write(Extent {
            kind: EXTENT_DATA,
            nr_pages,
            phys: 0,
        });
        count.write(1);
    }
    Status::OK
}

static TABLE: StoreTable = StoreTable {
    abi_version: ABI_VERSION,
    id_mask: ID_MASK,
    id_value: ID_VALUE,
    len,
    page_in,
};

#[unsafe(no_mangle)]
pub extern "C" fn twz_pager_store_register() -> *const StoreTable {
    &TABLE
}

#[cfg(test)]
mod tests {
    use twizzler_rt_abi::object::{MapFlags, ObjID, twz_rt_map_object};

    use super::*;

    #[test]
    fn loaded_store_objects() {
        for n in 1..=OBJECTS {
            let handle = twz_rt_map_object(ObjID::new(ID_VALUE | n as u128), MapFlags::READ)
                .expect("map store object");
            let len = object_len(n);
            for off in [PAGE_SIZE, PAGE_SIZE + 8, 2 << 20, len / 2, len - 8] {
                let off = off & !7;
                if off >= len {
                    continue;
                }
                let got = unsafe {
                    handle
                        .start()
                        .add(off as usize)
                        .cast::<u64>()
                        .read_volatile()
                };
                assert_eq!(got, word(n, off), "object {} offset {:#x}", n, off);
            }
        }
    }
}
