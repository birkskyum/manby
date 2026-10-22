//! Port of `config.hpp`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipType {
    Intersection = 0,
    Union,
    Difference,
    XOr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolygonType {
    Subject = 0,
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillType {
    EvenOdd = 0,
    NonZero,
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeSide {
    Left = 0,
    Right,
}

/// The `std::runtime_error` that wagyu throws. `what()` and `Display` give the same message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    message: &'static str,
}

impl Error {
    pub(crate) const fn new(message: &'static str) -> Self {
        Error { message }
    }

    pub fn what(&self) -> &str {
        self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;
