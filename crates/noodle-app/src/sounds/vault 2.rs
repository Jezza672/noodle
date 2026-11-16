//! Where a source's secret is kept: the token or password of a self-hosted
//! source.
//!
//! The system keychain where Noodle can use one (macOS and Windows), and the
//! preferences file everywhere else, or when the keychain refuses. The page
//! says which, so nobody is told their password is safer than it is. The
//! preferences file is not exported and not saved into projects.

use crate::prefs;

/// Where a secret ended up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    Keychain,
    PrefsFile,
}

impl Kept {
    pub fn name(self) -> &'static str {
        match self {
            Kept::Keychain => "the system keychain",
            Kept::PrefsFile => "the preferences file",
        }
    }
}

pub trait Vault {
    /// Keeps `secret` for the source `id`, and says where.
    fn keep(&self, id: &str, secret: &str) -> Kept;
    fn recall(&self, id: &str) -> Option<String>;
    fn forget(&self, id: &str);
    /// Where a secret goes when it is kept, before there is one to keep.
    fn home(&self) -> Kept;
}

/// The keychain if there is one, else the preferences.
pub struct System;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod keychain {
    const SERVICE: &str = "Noodle sources";

    fn entry(id: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, id).ok()
    }

    pub fn set(id: &str, secret: &str) -> bool {
        entry(id).is_some_and(|e| e.set_password(secret).is_ok())
    }

    pub fn get(id: &str) -> Option<String> {
        entry(id)?.get_password().ok()
    }

    pub fn delete(id: &str) {
        if let Some(e) = entry(id) {
            let _ = e.delete_credential();
        }
    }

    pub const AVAILABLE: bool = true;
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod keychain {
    pub fn set(_: &str, _: &str) -> bool {
        false
    }

    pub fn get(_: &str) -> Option<String> {
        None
    }

    pub fn delete(_: &str) {}

    pub const AVAILABLE: bool = false;
}

impl Vault for System {
    fn keep(&self, id: &str, secret: &str) -> Kept {
        if keychain::set(id, secret) {
            prefs::remember_source_secret(id, None);
            Kept::Keychain
        } else {
            prefs::remember_source_secret(id, Some(secret));
            Kept::PrefsFile
        }
    }

    fn recall(&self, id: &str) -> Option<String> {
        keychain::get(id).or_else(|| prefs::source_secret(id))
    }

    fn forget(&self, id: &str) {
        keychain::delete(id);
        prefs::remember_source_secret(id, None);
    }

    fn home(&self) -> Kept {
        if keychain::AVAILABLE {
            Kept::Keychain
        } else {
            Kept::PrefsFile
        }
    }
}

/// Secrets held in memory, for tests. It says it is the keychain.
#[cfg(test)]
#[derive(Default, Clone)]
pub struct Memory(pub std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>);

#[cfg(test)]
impl Vault for Memory {
    fn keep(&self, id: &str, secret: &str) -> Kept {
        self.0.lock().unwrap().insert(id.into(), secret.into());
        Kept::Keychain
    }

    fn recall(&self, id: &str) -> Option<String> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn forget(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }

    fn home(&self) -> Kept {
        Kept::Keychain
    }
}
