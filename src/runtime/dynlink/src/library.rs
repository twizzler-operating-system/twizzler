mod initialize;
pub(crate) mod internal;
mod load;
mod name;
mod relocate;

pub use load::LibraryLoader;
pub use name::*;
pub use relocate::SymbolResolver;

use self::internal::InternalLibrary;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct LibraryId(pub(crate) u64);

macro_rules! library_state_decl {
    ($name:ident, $sym:ty) => {
        #[derive(Debug, Clone, PartialEq, PartialOrd)]
        pub struct $name {
            int: InternalLibrary,
        }

        #[allow(dead_code)]
        impl $name {
            pub(crate) fn internal(&self) -> &InternalLibrary {
                &self.int
            }

            pub(crate) fn internal_mut(&mut self) -> &mut InternalLibrary {
                &mut self.int
            }
        }

        impl From<InternalLibrary> for $name {
            fn from(value: InternalLibrary) -> Self {
                Self { int: value }
            }
        }

        impl From<$name> for InternalLibrary {
            fn from(value: $name) -> Self {
                value.int
            }
        }
    };
}

library_state_decl!(UnloadedLibrary, UnrelocatedSymbol);
library_state_decl!(UnrelocatedLibrary, UnrelocatedSymbol);
library_state_decl!(UninitializedLibrary, RelocatedSymbol);
library_state_decl!(ReadyLibrary, RelocatedSymbol);

pub struct LibraryCollection<L> {
    pub(crate) root: Option<L>,
    pub(crate) deps: Vec<L>,
}

impl<L> From<(L, Vec<L>)> for LibraryCollection<L> {
    fn from(value: (L, Vec<L>)) -> Self {
        Self {
            root: Some(value.0),
            deps: value.1,
        }
    }
}

impl<L> Iterator for LibraryCollection<L> {
    type Item = L;

    fn next(&mut self) -> Option<Self::Item> {
        self.root.take().or_else(|| self.deps.pop())
    }
}
