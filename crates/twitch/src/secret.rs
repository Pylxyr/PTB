use std::fmt;

use serde::{Deserialize, Serialize};

/// A token or key. It prints as `[redacted]` so it cannot end up in a log or a panic message by accident;
/// the only way to read it is the explicit [`Secret::expose`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_show_the_value() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert!(!format!("{:?}", Some(&s)).contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn it_serialises_as_a_plain_string() {
        let s = Secret::new("abc");
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"abc\"");
        assert_eq!(serde_json::from_str::<Secret>("\"abc\"").unwrap(), s);
    }
}
