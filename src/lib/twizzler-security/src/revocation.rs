/// Specifies when a Capability is invalid.
/// Currenty is a time in ns from unix epoch but
/// plan to change later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Revoc {
    inner: u128,
}

//TODO: impl all the revoc stuff (comparison, creating, allat)

impl Revoc {
    pub fn new(time: u128) -> Self {
        Revoc { inner: time }
    }

    /// Represents a revocation as an owned array of bytes
    pub fn to_bytes(&self) -> [u8; 16] {
        self.inner.to_le_bytes()
    }
}

impl Default for Revoc {
    fn default() -> Self {
        //TODO: come up with a sensible default
        Self { inner: 0 }
    }
}
