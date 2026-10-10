//! Content-addressed cache keys.
//!
//! A [`CacheKey`] names a render by what went into it. Keys are built
//! Merkle-style: a node's key hashes its own description together with the
//! keys of everything upstream, so any change anywhere upstream gives a new
//! key and a stale render can never be served (see "Caching" in
//! `docs/ARCHITECTURE.md`). This module only supplies the hashing; deciding
//! what goes into a key is the compiler's job.

use std::fmt;

use crate::{Config, TempoMap, Value};

/// A 256-bit content hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CacheKey([u8; 32]);

impl CacheKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The hash of everything `reader` yields: how a file's contents become a
    /// key.
    pub fn of_reader(reader: impl std::io::Read) -> std::io::Result<Self> {
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(reader)?;
        Ok(Self(*hasher.finalize().as_bytes()))
    }

    /// 64 lowercase hex digits; the file name a store uses.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The inverse of [`to_hex`](Self::to_hex).
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Debug for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CacheKey({})", self.to_hex())
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Builds a [`CacheKey`] from a sequence of fields. Every field is tagged
/// with its type and, for text and bytes, its length, so different field
/// sequences can't produce the same byte stream.
pub struct KeyBuilder {
    hasher: blake3::Hasher,
}

impl KeyBuilder {
    /// `domain` separates unrelated kinds of key, e.g. `"node-render"`.
    pub fn new(domain: &str) -> Self {
        let mut builder = Self {
            hasher: blake3::Hasher::new(),
        };
        builder.str(domain);
        builder
    }

    pub fn str(&mut self, text: &str) -> &mut Self {
        self.hasher.update(b"s");
        self.hasher.update(&(text.len() as u64).to_le_bytes());
        self.hasher.update(text.as_bytes());
        self
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.hasher.update(b"b");
        self.hasher.update(&(bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
        self
    }

    pub fn u64(&mut self, value: u64) -> &mut Self {
        self.hasher.update(b"u");
        self.hasher.update(&value.to_le_bytes());
        self
    }

    pub fn i64(&mut self, value: i64) -> &mut Self {
        self.hasher.update(b"i");
        self.hasher.update(&value.to_le_bytes());
        self
    }

    /// Hashes the bit pattern, so `0.0` and `-0.0` differ and a NaN is
    /// stable.
    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.hasher.update(b"f");
        self.hasher.update(&value.to_bits().to_le_bytes());
        self
    }

    pub fn f64(&mut self, value: f64) -> &mut Self {
        self.hasher.update(b"d");
        self.hasher.update(&value.to_bits().to_le_bytes());
        self
    }

    pub fn bool(&mut self, value: bool) -> &mut Self {
        self.hasher.update(b"o");
        self.hasher.update(&[u8::from(value)]);
        self
    }

    /// A node's config: every setting by key, with its type.
    pub fn config(&mut self, config: &Config) -> &mut Self {
        self.u64(config.iter().count() as u64);
        for (key, value) in config.iter() {
            self.str(key);
            match value {
                Value::Bool(v) => self.bool(*v),
                Value::Int(v) => self.i64(*v),
                Value::Float(v) => self.f64(*v),
                Value::Text(v) => self.str(v),
            };
        }
        self
    }

    /// Every tempo and time signature change.
    pub fn tempo_map(&mut self, map: &TempoMap) -> &mut Self {
        self.u64(map.tempos().len() as u64);
        for tempo in map.tempos() {
            self.i64(tempo.tick.0).f64(tempo.bpm);
        }
        self.u64(map.signatures().len() as u64);
        for change in map.signatures() {
            self.u64(u64::from(change.bar))
                .u64(u64::from(change.signature.numerator))
                .u64(u64::from(change.signature.denominator));
        }
        self
    }

    /// Mixes in another key, e.g. an upstream node's.
    pub fn key(&mut self, key: &CacheKey) -> &mut Self {
        self.hasher.update(b"k");
        self.hasher.update(&key.0);
        self
    }

    pub fn finish(&self) -> CacheKey {
        CacheKey(*self.hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(f: impl FnOnce(&mut KeyBuilder)) -> CacheKey {
        let mut b = KeyBuilder::new("test");
        f(&mut b);
        b.finish()
    }

    #[test]
    fn same_fields_same_key() {
        let a = key(|b| {
            b.str("gain").f32(0.5).u64(3);
        });
        let b = key(|b| {
            b.str("gain").f32(0.5).u64(3);
        });
        assert_eq!(a, b);
    }

    #[test]
    fn any_field_change_changes_the_key() {
        let base = key(|b| {
            b.str("gain").f32(0.5);
        });
        assert_ne!(
            base,
            key(|b| {
                b.str("gain").f32(0.5001);
            })
        );
        assert_ne!(
            base,
            key(|b| {
                b.str("gai").f32(0.5);
            })
        );
        assert_ne!(base, KeyBuilder::new("other").str("gain").f32(0.5).finish());
    }

    #[test]
    fn every_field_type_has_its_own_tag() {
        let keys = [
            key(|b| {
                b.u64(1);
            }),
            key(|b| {
                b.i64(1);
            }),
            key(|b| {
                b.bool(true);
            }),
            key(|b| {
                b.f32(1.0);
            }),
            key(|b| {
                b.f64(1.0);
            }),
            key(|b| {
                b.str("a");
            }),
            key(|b| {
                b.bytes(b"a");
            }),
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
            }
        }
        let signed = key(|b| {
            b.i64(-1);
        });
        let unsigned = key(|b| {
            b.u64(u64::MAX);
        });
        assert_ne!(signed, unsigned);
    }

    #[test]
    fn field_boundaries_matter() {
        let a = key(|b| {
            b.str("ab").str("c");
        });
        let b = key(|b| {
            b.str("a").str("bc");
        });
        assert_ne!(a, b);
    }

    #[test]
    fn upstream_keys_chain() {
        let up1 = key(|b| {
            b.str("osc").f32(440.0);
        });
        let up2 = key(|b| {
            b.str("osc").f32(441.0);
        });
        let down = |up: &CacheKey| {
            key(|b| {
                b.str("gain").key(up);
            })
        };
        assert_ne!(down(&up1), down(&up2));
    }

    #[test]
    fn hex_round_trips() {
        let k = key(|b| {
            b.u64(7);
        });
        assert_eq!(CacheKey::from_hex(&k.to_hex()), Some(k));
        assert_eq!(CacheKey::from_hex("zz"), None);
        assert_eq!(CacheKey::from_hex(&"é".repeat(32)), None);
    }
}
