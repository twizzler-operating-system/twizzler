//! The interface between the pager and object stores it loads from libraries.
//!
//! At start-up the pager loads every initrd library whose name starts with [LIB_PREFIX] and calls
//! its [REGISTER_SYMBOL]. Everything that crosses is `repr(C)` plain data, since a store may be
//! built in another workspace against other versions of the crates the pager uses.

pub const ABI_VERSION: u32 = 1;
pub const LIB_PREFIX: &str = "libpagerstore_";
pub const REGISTER_SYMBOL: &[u8] = b"twz_pager_store_register";
pub const PAGE_SIZE: usize = 0x1000;

pub type RegisterFn = unsafe extern "C" fn() -> *const StoreTable;

#[repr(C)]
pub struct StoreTable {
    pub abi_version: u32,
    /// The store serves every object whose ID satisfies `id & id_mask == id_value`.
    pub id_mask: u128,
    pub id_value: u128,
    /// Byte length of object `id`, counted from offset 0.
    pub len: unsafe extern "C" fn(id: u128, len: *mut u64) -> Status,
    /// Describe object pages `[start_page, start_page + nr_pages)` in order, as at most `cap`
    /// extents written to `out`, setting `*count` to how many. May cover fewer pages than asked
    /// for, but not none. `data` holds `nr_pages * PAGE_SIZE` bytes; a [EXTENT_DATA] extent's
    /// contents go at its page offset from `start_page`.
    pub page_in: unsafe extern "C" fn(
        id: u128,
        start_page: u64,
        nr_pages: u32,
        data: *mut u8,
        out: *mut Extent,
        cap: u32,
        count: *mut u32,
    ) -> Status,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status(pub u32);

impl Status {
    pub const OK: Self = Self(0);
    pub const NOT_FOUND: Self = Self(1);
    pub const IO_ERROR: Self = Self(2);
}

/// The pages are in the `data` buffer; the pager copies them into memory of its own.
pub const EXTENT_DATA: u32 = 0;
/// The pages are physical memory at [Extent::phys], mapped as they are and never reclaimed.
pub const EXTENT_PHYS: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extent {
    pub kind: u32,
    pub nr_pages: u32,
    pub phys: u64,
}
