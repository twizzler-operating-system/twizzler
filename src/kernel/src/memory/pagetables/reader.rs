use crate::arch::address::{PhysAddr, VirtAddr};

use super::{Mapper, MappingCursor, MappingSettings};

/// Iterator for reading mapping information. Will not cross non-canonical address boundaries.
pub struct MapReader<'a> {
    mapper: &'a Mapper,
    cursor: Option<MappingCursor>,
}

impl<'a> MapReader<'a> {
    pub fn coalesce(self) -> MapCoalescer<'a> {
        MapCoalescer {
            reader: self,
            last: None,
        }
    }
}

impl<'a> Iterator for MapReader<'a> {
    type Item = MapInfo;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(cursor) = self.cursor {
                if cursor.remaining() == 0 {
                    return None;
                }
                let info = self.mapper.do_read_map(&cursor);
                match info {
                    Ok(info) => {
                        self.cursor = cursor.advance(info.psize);
                        return Some(info);
                    }
                    Err(skip) => {
                        self.cursor = cursor.advance(skip);
                        continue;
                    }
                }
            } else {
                return None;
            }
        }
    }
}

pub struct MapCoalescer<'a> {
    reader: MapReader<'a>,
    last: Option<MapInfo>,
}

impl<'a> Iterator for MapCoalescer<'a> {
    type Item = MapInfo;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let next = self.reader.next();
            if let Some(next) = next {
                if let Some(last) = &mut self.last {
                    if let Ok(last_next) = last.vaddr().offset(last.len()) && let Ok(last_next_phys) = last.paddr().offset(last.len()) {
                        if last_next == next.vaddr() && last.settings() == next.settings() && last_next_phys == next.paddr() {
                            last.psize += next.len();
                            continue;
                        }
                    }

                    let ret = last.clone();
                    *last = next;
                    return Some(ret);
                } else {
                    self.last = Some(next);
                }
            } else {
                return self.last.take();
            }
        }
    }
}

#[derive(Debug, PartialEq, PartialOrd, Clone)]
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
    pub fn len(&self) -> usize {
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
