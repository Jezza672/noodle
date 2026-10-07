//! Node configuration: settings that change a node's ports or signal shapes,
//! such as how many inputs a mixer has. Changing config rebuilds the node.
//! Every other setting is a parameter, which can be modulated.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }
}

/// A node's config values by key. Missing keys take the node type's defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Config(BTreeMap<String, Value>);

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Returns the previous value.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        self.0.insert(key.into(), value)
    }

    /// Returns the removed value.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    pub fn with(mut self, key: impl Into<String>, value: Value) -> Self {
        self.set(key, value);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
