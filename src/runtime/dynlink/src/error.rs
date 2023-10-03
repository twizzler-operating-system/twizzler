use std::sync::{LockResult, PoisonError};

use thiserror::Error;
use twizzler_abi::syscall::ObjectCreateError;
#[derive(Debug, Error)]
pub enum DynlinkError {
    #[error("unknown")]
    Unknown,
    #[error("{}", .0.iter().map(|e| e.to_string()).fold(String::new(), |a, b| a + &b + "\n"))]
    Collection(Vec<DynlinkError>),
    #[error("not found: {name}")]
    NotFound { name: String },
    #[error("name already exists: {name}")]
    AlreadyExists { name: String },
    #[error("parse failed: {err}")]
    ParseError {
        #[from]
        err: elf::ParseError,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl<T> From<PoisonError<T>> for DynlinkError {
    fn from(value: PoisonError<T>) -> Self {
        Self::Other(anyhow::anyhow!(value.to_string()))
    }
}

impl From<Vec<anyhow::Error>> for DynlinkError {
    fn from(value: Vec<anyhow::Error>) -> Self {
        Self::Collection(value.into_iter().map(|e| e.into()).collect())
    }
}

impl FromIterator<anyhow::Error> for DynlinkError {
    fn from_iter<T: IntoIterator<Item = anyhow::Error>>(iter: T) -> Self {
        Self::Collection(iter.into_iter().map(|e| e.into()).collect())
    }
}

impl From<Vec<DynlinkError>> for DynlinkError {
    fn from(value: Vec<DynlinkError>) -> Self {
        let mut new = vec![];
        for v in value {
            match v {
                DynlinkError::Collection(mut list) => {
                    new.append(&mut list);
                }
                v => new.push(v.into()),
            }
        }
        Self::Collection(new)
    }
}

pub trait ECollector<T> {
    fn ecollect<I>(self) -> Result<I, DynlinkError>
    where
        I: std::iter::FromIterator<T>;
}

impl<T, U, E> ECollector<T> for U
where
    U: Iterator<Item = Result<T, E>>,
    E: std::convert::Into<DynlinkError>,
{
    #[allow(clippy::redundant_closure_call)]
    fn ecollect<I>(self) -> Result<I, DynlinkError>
    where
        I: std::iter::FromIterator<T>,
    {
        let (good, bad): (I, Vec<DynlinkError>) = (|(g, b): (Vec<_>, Vec<_>)| {
            (
                g.into_iter()
                    .map(|res| match res {
                        Ok(x) => x,
                        Err(_) => panic!(),
                    })
                    .collect(),
                b.into_iter()
                    .map(|res| match res {
                        Ok(_) => panic!(),
                        Err(e) => e,
                    })
                    .map(Into::into)
                    .collect(),
            )
        })(self.partition(Result::is_ok));

        if bad.is_empty() {
            Ok(good)
        } else {
            Err(DynlinkError::Collection(bad))
        }
    }
}
