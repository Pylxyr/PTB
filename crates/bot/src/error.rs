//! One error type for the command line: a message that already says what was being done.

use std::fmt;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Adds "what was being done" in front of an underlying error: `reading the token file: no such file`.
pub trait Context<T> {
    fn context(self, what: &str) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: &str) -> Result<T> {
        self.map_err(|e| Error(format!("{what}: {e}")))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, what: &str) -> Result<T> {
        self.ok_or_else(|| Error(what.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_puts_what_was_happening_first() {
        let failed: std::result::Result<(), &str> = Err("disk full");
        assert_eq!(
            failed.context("saving").unwrap_err().to_string(),
            "saving: disk full"
        );
        assert_eq!(
            None::<u8>.context("nothing there").unwrap_err().to_string(),
            "nothing there"
        );
        assert_eq!(Some(3).context("unused").unwrap(), 3);
    }
}
