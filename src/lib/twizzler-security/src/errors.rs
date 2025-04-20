#[derive(Debug)]
pub enum SecError {
    InvalidFlags,

    InvalidScheme,

    InvalidVerifyKey,
    InvalidSigningKey,

    InvalidSignature,

    OutsideBounds,
    Unaligned,
}
