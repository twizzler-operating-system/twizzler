use crate::{CapError, SigningScheme};

mod sig;
mod sign;
mod verify;
pub use sig::*;
pub use sign::*;
pub use verify::*;

const MAX_KEY_SIZE: usize = 512;
const MAX_SIG_SIZE: usize = 128;

//TODO: write docs describing each of these error cases
#[derive(Debug, Clone, Copy)]
pub enum KeyError {
    InvalidKeyLength,
    InvalidScheme,
}
