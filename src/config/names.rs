//! The names the config hands out, so a model is never mistaken for a client or a placement.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The name clients give to ask for a model.
// wire format: state.json holds it, so changing this is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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
// wire format: state.json holds it, so changing this is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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

/// What the config calls one way a model can run.
// wire format: state.json holds it, so changing this is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct PlacementName(String);

impl PlacementName {
    /// The name as written in the config.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PlacementName {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

impl From<String> for PlacementName {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl fmt::Display for PlacementName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
