use fatfs::Extent;
use std::hash::Hash;

#[derive(Clone, Debug)]
pub struct WrappedExtent(Extent);

impl PartialEq for WrappedExtent {
    fn eq(&self, other: &Self) -> bool {
        self.0.offset == other.0.offset && self.0.size == other.0.size
    }
}
impl Eq for WrappedExtent {}

impl From<Extent> for WrappedExtent {
    fn from(value: Extent) -> Self {
        WrappedExtent(value)
    }
}

impl Hash for WrappedExtent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.offset.hash(state);
        self.0.size.hash(state);
    }
}
