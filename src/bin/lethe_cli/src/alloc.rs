use allocator::Allocator;
use num_traits::PrimInt;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, hash::Hash, ops::AddAssign};
use thiserror::Error;

#[derive(Serialize, Deserialize, Default)]
pub struct SequentialAllocator<T: PrimInt + Hash> {
    curr: T,
    reserved: HashSet<T>,
}

impl<T: PrimInt + Hash> SequentialAllocator<T> {
    pub fn new() -> Self {
        Self {
            curr: T::zero(),
            reserved: HashSet::new(),
        }
    }
}

impl<T> Allocator for SequentialAllocator<T>
where
    T: PrimInt + AddAssign + Hash,
{
    type Id = T;
    type Error = Error;

    fn alloc(&mut self) -> Result<Self::Id, Self::Error> {
        if self.curr == T::max_value() {
            Err(Error::OutOfIds)
        } else {
            while self.reserved.contains(&self.curr) {
                self.curr += T::one();
            }
            let id = self.curr;
            self.curr += T::one();
            Ok(id)
        }
    }

    fn dealloc(&mut self, id: Self::Id) -> Result<(), Self::Error> {
        self.reserved.remove(&id);
        Ok(())
    }

    fn reserve(&mut self, id: Self::Id) -> Result<(), Self::Error> {
        self.reserved.insert(id);
        Ok(())
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("no more IDs to allocate")]
    OutOfIds,
}