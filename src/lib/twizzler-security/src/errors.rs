#[derive(Debug)]
pub enum CapError {
    InvalidSignature,
    InvalidVerifyKey,
    InvalidFlags,
    InvalidPrivateKey,
    CorruptedSignature,
}
