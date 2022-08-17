use std::ops::Index;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct PhysAddr(u64);

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct PhysInfo {
    addr: PhysAddr,
    offset: usize,
}

pub struct DmaPinIter<'a> {
    pin: &'a [PhysInfo],
    idx: usize,
}

pub struct DmaPin<'a> {
    backing: &'a [PhysInfo],
}

impl PhysInfo {
    pub fn addr(&self) -> PhysAddr {
        self.addr
    }

    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl From<PhysAddr> for u64 {
    fn from(p: PhysAddr) -> Self {
        p.0
    }
}

impl<'a> IntoIterator for DmaPin<'a> {
    type Item = PhysInfo;

    type IntoIter = DmaPinIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        DmaPinIter {
            pin: self.backing,
            idx: 0,
        }
    }
}

impl<'a> Iterator for DmaPinIter<'a> {
    type Item = PhysInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.pin.len() {
            None
        } else {
            let ret = self.pin[self.idx];
            self.idx += 1;
            Some(ret)
        }
    }
}

impl<'a> Index<usize> for DmaPin<'a> {
    type Output = PhysInfo;

    fn index(&self, index: usize) -> &Self::Output {
        &self.backing[index]
    }
}
