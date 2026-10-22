//! Errors. Each carries the exit status tippecanoe would use for the same
//! problem, so the command can exit the way tippecanoe does.

use std::fmt;

#[derive(Debug)]
pub struct Error {
    pub message: String,
    /// One of tippecanoe's exit statuses (`tippecanoe::errors::EXIT_*`).
    pub exit_code: i32,
}

impl Error {
    pub fn new(exit_code: i32, message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            exit_code,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
