#[derive(Debug, Clone)]
pub enum Error {
    Unseeded,
    TooMuchData,
    TooLittleData,
    PoolNumTooBig,
}
