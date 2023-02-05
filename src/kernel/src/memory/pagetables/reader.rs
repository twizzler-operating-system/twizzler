use crate::arch::{
    address::{PhysAddr, VirtAddr},
    memory::pagetables::Table,
};

use super::{Mapper, MappingCursor, MappingSettings};

/// Iterator for reading mapping information. Will not cross non-canonical address boundaries.
pub struct MapReader<'a> {
    mapper: &'a Mapper,
    cursor: Option<MappingCursor>,
}

impl<'a> Iterator for MapReader<'a> {
    type Item = MapInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(cursor) = self.cursor {
            if cursor.remaining() == 0 {
                return None;
            }
            let info = self.mapper.do_read_map(&cursor);
            if let Some(info) = info {
                self.cursor = cursor.advance(info.psize);
                Some(info)
            } else {
                self.cursor = cursor.advance(Table::level_to_page_size(0));
                self.next()
            }
        } else {
            None
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd)]
/// Information about a specific mapping.
pub struct MapInfo {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    settings: MappingSettings,
    psize: usize,
}

impl Mapper {
    /// Create a [MapReader] that can be used to iterate over the region specified by the mapping cursor. If the mapping
    /// cursor includes a non-canonical region, the reader will stop early.
    pub fn readmap(&self, cursor: MappingCursor) -> MapReader<'_> {
        MapReader {
            mapper: self,
            cursor: Some(cursor),
        }
    }
}

impl MapInfo {
    pub(super) fn new(
        vaddr: VirtAddr,
        paddr: PhysAddr,
        settings: MappingSettings,
        psize: usize,
    ) -> Self {
        Self {
            vaddr,
            paddr,
            settings,
            psize,
        }
    }

    /// Virtual address of the mapping.
    pub fn vaddr(&self) -> VirtAddr {
        self.vaddr
    }

    /// Length of this individual mapping (corresponds to the length of physical and virtual memory covered by this mapping).
    pub fn psize(&self) -> usize {
        self.psize
    }

    /// Map settings.
    pub fn settings(&self) -> &MappingSettings {
        &self.settings
    }

    /// Physical address of the mapping.
    pub fn paddr(&self) -> PhysAddr {
        self.paddr
    }
}
