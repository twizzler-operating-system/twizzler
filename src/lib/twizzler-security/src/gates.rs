#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Gates are a range into an object that a
/// `Cap` / `Del` provides access to.
/// Typically Gates are set to the entire
/// object, but can be defined to the byte-level.
/// This primitive is used to support Secure API Calls
/// TODO: link more info about secure api calls
pub struct Gate {
    /// The offset into the object that we provide permissions for
    pub offset: u64,
    /// How far that area should strech, from the offset
    pub length: u64,
    /// The alignment
    pub align: u64,
}

/// The maximum length of an object.
static MAX_LEN: u64 = 1e9 as u64;

impl Gate {
    /// Create a new Gate
    pub fn new(offset: u64, length: u64, align: u64) -> Self {
        Gate {
            offset,
            length,
            align,
        }
    }
}

impl Default for Gate {
    fn default() -> Self {
        //NOTE: verify with daniel that these are the default values for gates
        Gate {
            offset: 0,
            length: MAX_LEN,
            align: 1,
        }
    }
}
