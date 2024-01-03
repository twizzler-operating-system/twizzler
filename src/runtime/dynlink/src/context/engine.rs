use crate::{
    library::{BackingData, UnloadedLibrary},
    DynlinkError,
};

pub trait ContextEngine {
    type Backing: BackingData;

    fn load_segments(
        &mut self,
        src: &Self::Backing,
        ld: &[LoadDirective],
    ) -> Result<Vec<Self::Backing>, DynlinkError>;

    fn load_object(&mut self, unlib: &UnloadedLibrary) -> Result<Self::Backing, DynlinkError>;
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct LoadDirective {
    pub load_flags: LoadFlags,
    pub vaddr: usize,
    pub memsz: usize,
    pub offset: usize,
    pub align: usize,
    pub filesz: usize,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct LoadFlags: u32 {
        const TARGETS_DATA = 1;
    }
}
