pub use std::{alloc::LayoutError, io::Error as IoError};
use std::{error, fmt};

#[derive(Debug)]
pub enum Error {
  LayoutError(LayoutError),
  IoError(IoError),
}

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SensitiveData error")
  }
}

impl error::Error for Error {
  fn source(&self) -> Option<&(dyn error::Error + 'static)> {
    match self {
      Error::LayoutError(ref l) => Some(l),
      Error::IoError(ref e) => Some(e),
    }
  }
}

impl From<IoError> for Error {
  fn from(other: IoError) -> Error {
    Error::IoError(other)
  }
}

impl From<LayoutError> for Error {
  fn from(other: LayoutError) -> Error {
    Error::LayoutError(other)
  }
}
