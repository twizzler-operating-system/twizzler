use twizzler_runtime_api::{DebugRuntime, Library};

use super::MinimalRuntime;

impl DebugRuntime for MinimalRuntime {
    type LibType = MinimalLibrary;

    type LibIterator = MinimalLibraryIter;

    fn iter_libs(&self) -> Self::LibIterator {
        MinimalLibraryIter {}
    }
}

impl Library for MinimalLibrary {}

pub struct MinimalLibrary {}

pub struct MinimalLibraryIter {}

impl Iterator for MinimalLibraryIter {
    type Item = MinimalLibrary;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}
