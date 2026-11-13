use serde::{Deserialize, Serialize};

/// The SPDX ids a pack may name. A short list on purpose: each one has a known
/// answer to "must I credit?" and "may it be redistributed?".
pub const SPDX_IDS: &[&str] = &[
    "CC0-1.0",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
    "CC-BY-NC-4.0",
    "CC-BY-3.0",
    "MIT",
];

/// What a sample may be used for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Licence {
    /// Use in your own projects, commercial or not; do not redistribute.
    #[default]
    #[serde(rename = "default")]
    Default,
    /// An SPDX identifier from [`SPDX_IDS`].
    #[serde(rename = "spdx")]
    Spdx(String),
    /// Free text supplied by the author, shown as is.
    #[serde(rename = "custom")]
    Custom(String),
}

impl Licence {
    /// Whether using the sample obliges you to credit its author.
    pub fn requires_credit(&self) -> bool {
        match self {
            Licence::Spdx(id) => id.starts_with("CC-BY") || id == "MIT",
            Licence::Default | Licence::Custom(_) => false,
        }
    }

    /// Whether the sample may be passed on to others. Custom text is assumed
    /// not to, since the default for anything we cannot read is the cautious one.
    pub fn allows_redistribution(&self) -> bool {
        match self {
            Licence::Spdx(_) => true,
            Licence::Default | Licence::Custom(_) => false,
        }
    }

    /// A short label for the browser's licence filter.
    pub fn label(&self) -> String {
        match self {
            Licence::Default => "Default".into(),
            Licence::Spdx(id) => id.clone(),
            Licence::Custom(_) => "Custom".into(),
        }
    }
}
