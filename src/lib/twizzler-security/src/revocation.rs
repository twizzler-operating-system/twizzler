#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Revoc {
    inner: u128,
}

//TODO: impl all the revoc stuff (comparison, creating, allat)

impl Revoc {
    // for hashing
    pub fn serialize(&self) -> [u8; 16] {
        self.inner.to_le_bytes()
    }
}
impl Default for Revoc {
    fn default() -> Self {
        //TODO: come up with a sensible default
        Self { inner: 0 }
    }
}
