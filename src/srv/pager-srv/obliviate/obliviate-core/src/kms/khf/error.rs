use std::io;

use thiserror::Error;

use crate::wal;

#[derive(Error, Debug)]
pub enum Error<G, C> {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Serde(#[from] bincode::Error),

    #[error(transparent)]
    WAL(#[from] wal::SecureWALError<G, C, io::Error>),

    #[error("oneshot crypt io error")]
    OneshotCryptIo,

    #[error("key doesn't exist")]
    NonExistentKey,

    #[error("failed to persist state")]
    Persist,

    #[error("failed to load state")]
    Load,

    #[error("failed to load inode KHF")]
    LoadObjectKhf,

    #[error("lru-mem cache: {0}")]
    LruMem(String),

    #[error("no evictable inode KHF")]
    EvictionImpossible,
}

pub type Result<T, G, C> = std::result::Result<T, Error<G, C>>;
