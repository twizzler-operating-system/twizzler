#[derive(Debug)]
pub enum SecError {
    InvalidFlags,

    InvalidScheme,

    InvalidVerifyKey,
    InvalidPrivateKey,

    InvalidSignature,

    OutsideBounds,
    Unailgned
}
