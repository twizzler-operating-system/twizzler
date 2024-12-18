use crate::{marker::Invariant, ptr::InvPtr};

pub struct InvBox<T: Invariant> {
    raw: InvPtr<T>,
}
