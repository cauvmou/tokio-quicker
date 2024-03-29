use std::fmt::{Display, Formatter};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    IdAlreadyTaken(u64),
    IoError(std::io::Error),
    BackendError(quiche::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::IdAlreadyTaken(id) => {
                write!(f, "Id: {id} is already taken by another open stream.")
            }
            Error::IoError(error) => write!(f, "{error}"),
            Error::BackendError(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::IoError(value)
    }
}

impl From<std::io::ErrorKind> for Error {
    fn from(value: std::io::ErrorKind) -> Self {
        Self::IoError(value.into())
    }
}

impl From<quiche::Error> for Error {
    fn from(value: quiche::Error) -> Self {
        Self::BackendError(value)
    }
}
