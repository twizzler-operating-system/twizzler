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

    fn load_object<N>(
        &mut self,
        unlib: &UnloadedLibrary,
        mut n: N,
    ) -> Result<Self::Backing, DynlinkError>
    where
        N: FnMut(&str) -> Option<Self::Backing>,
    {
        n(&unlib.name).ok_or_else(|| {
            DynlinkError::new(crate::DynlinkErrorKind::NameNotFound {
                name: unlib.name.clone(),
            })
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct LoadDirective {
    pub load_flags: LoadFlags,
    pub vaddr: usize,
    pub memsz: usize,
    pub offset: usize,
    pub align: usize,
    pub filesz: usize,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
    pub struct LoadFlags: u32 {
        const TARGETS_DATA = 1;
    }
}
