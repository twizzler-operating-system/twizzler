#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    Other,
    InvalidName,
    NotFound,
    NotNamespace,
    NotFile,
}

impl ErrorKind {
    pub fn as_str(&self) -> &'static str {
        use ErrorKind::*;
        match self {
            Other => "Other error",
            InvalidName => "Invalid name",
            NotFound => "Name was not found",
            NotNamespace => "Name isn't a namespace",
            NotFile => "Name is not a file",
        }
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.write_str(self.as_str())
    }
}

impl Into<std::io::ErrorKind> for ErrorKind {
    fn into(self) -> std::io::ErrorKind {
        match self {
            ErrorKind::Other => std::io::ErrorKind::Other,
            ErrorKind::InvalidName => std::io::ErrorKind::InvalidFilename,
            ErrorKind::NotFound => std::io::ErrorKind::NotFound,
            ErrorKind::NotNamespace => std::io::ErrorKind::NotADirectory,
            ErrorKind::NotFile => std::io::ErrorKind::InvalidFilename,
        }
    }
}

impl Into<twizzler_rt_abi::fd::OpenError> for ErrorKind {
    fn into(self) -> twizzler_rt_abi::fd::OpenError {
        match self {
            ErrorKind::Other => twizzler_rt_abi::fd::OpenError::Other,
            ErrorKind::InvalidName => twizzler_rt_abi::fd::OpenError::InvalidArgument,
            ErrorKind::NotFound => twizzler_rt_abi::fd::OpenError::LookupFail,
            ErrorKind::NotNamespace => twizzler_rt_abi::fd::OpenError::InvalidArgument,
            ErrorKind::NotFile => twizzler_rt_abi::fd::OpenError::InvalidArgument,
        }
    }
}

pub type Result<T> = std::result::Result<T, ErrorKind>;
