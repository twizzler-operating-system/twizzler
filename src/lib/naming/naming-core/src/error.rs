#[derive(PartialEq, Eq, Debug)]
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
            Other => "other error",
            InvalidName => "Invalid Name",
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

pub type Result<T> = std::result::Result<T, ErrorKind>;

