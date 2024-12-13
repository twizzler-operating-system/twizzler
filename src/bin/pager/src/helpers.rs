use twizzler_abi::pager::{
    PhysRange, ObjectRange
};

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

    return PhysRange { start: start, end: end }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_physrange_to_pages() {
        let range = PhysRange { start: 0, end: 8192 };
        assert_eq!(physrange_to_pages(&range), 2);

        let range = PhysRange { start: 0, end: 4095 };
        assert_eq!(physrange_to_pages(&range), 1);

        let range = PhysRange { start: 0, end: 0 };
        assert_eq!(physrange_to_pages(&range), 0);

        let range = PhysRange { start: 4096, end: 8192 };
        assert_eq!(physrange_to_pages(&range), 2);
    }

    #[test]
    fn test_objectrange_to_page_number() {
        let range = ObjectRange { start: 0, end: 4096 };
        assert_eq!(objectrange_to_page_number(&range), Some(0));

        let range = ObjectRange { start: 4096, end: 8192 };
        assert_eq!(objectrange_to_page_number(&range), Some(1));

        let range = ObjectRange { start: 0, end: 8192 }; // Invalid range for one page
        assert_eq!(objectrange_to_page_number(&range), None);

        let range = ObjectRange { start: 8192, end: 12288 };
        assert_eq!(objectrange_to_page_number(&range), Some(2));
    }
}

