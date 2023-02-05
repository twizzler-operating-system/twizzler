use crate::arch::address::{PhysAddr, VirtAddr};

use super::{Mapper, MappingCursor, MappingSettings, Table};

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
pub struct MapInfo {
    vaddr: VirtAddr,
    paddr: PhysAddr,
    settings: MappingSettings,
    psize: usize,
}

impl Mapper {
    pub fn readmap(&self, cursor: MappingCursor) -> MapReader<'_> {
        MapReader {
            mapper: self,
            cursor: Some(cursor),
        }
    }
}

impl MapInfo {
    pub fn new(vaddr: VirtAddr, paddr: PhysAddr, settings: MappingSettings, psize: usize) -> Self {
        Self {
            vaddr,
            paddr,
            settings,
            psize,
        }
    }

    pub fn vaddr(&self) -> VirtAddr {
        self.vaddr
    }

    pub fn psize(&self) -> usize {
        self.psize
    }

    pub fn settings(&self) -> &MappingSettings {
        &self.settings
    }
}
