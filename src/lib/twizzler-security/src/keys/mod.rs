use crate::SigningScheme;

mod sig;
mod sign;
mod verify;
pub use sig::*;
pub use sign::*;
pub use verify::*;

const MAX_KEY_SIZE: usize = 512;
