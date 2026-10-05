//! The two names the config hands out, so a model is never mistaken for a client.

use core::fmt;

/// The name clients give to ask for a model.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ModelName(String);

impl ModelName {
    /// The name as written in the config.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ModelName {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl From<String> for ModelName {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl fmt::Display for ModelName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the dog calls a client.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ClientName(String);

impl ClientName {
    /// The name as written in the config.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ClientName {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl From<String> for ClientName {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl fmt::Display for ClientName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
