use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error<IO, C, G, KMS>
where
    IO: std::error::Error,
    C: std::error::Error,
    G: std::error::Error,
    KMS: std::error::Error,
{
    #[error(transparent)]
    IO(IO),

    #[error(transparent)]
    Crypter(C),

    #[error(transparent)]
    IV(G),

    #[error(transparent)]
    KMS(KMS),

    #[error("failed to persist metadata write-ahead log")]
    MetadataWALPersist,

    #[error("failed to persist data write-ahead log")]
    DataWALPersist,

    #[error("failed precrypter io")]
    PreCrypt,
}
