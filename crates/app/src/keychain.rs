//! TDLib database encryption key, stored in the macOS Keychain via the
//! `keyring` crate; generated on first run and never written to disk in
//! plaintext (spec §9.3). Also the TDLib database directory itself.
//!
//! `tgt-app` has no library target, so `db_key`/`td_database_dir` have no
//! reachability root until the task that wires TDLib startup (docs/plan.md
//! T14+) calls them from `main.rs`/`td_runtime.rs`. `#![allow(dead_code)]`
//! covers that gap; every item it silences is exercised by this module's
//! own tests.

#![allow(dead_code)]

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

use color_eyre::eyre::{self, Context};
use etcetera::BaseStrategy;

const SERVICE: &str = "telegram-tui";
const DB_KEY_USER: &str = "db-encryption-key";
const APP_DIR: &str = "telegram-tui";
const TD_SUBDIR: &str = "td";

/// Gets the 32-byte TDLib database encryption key from the macOS Keychain,
/// generating and storing a fresh random one on first run. The key is
/// stored hex-encoded (Keychain entries are UTF-8 strings); it is never
/// held anywhere else, and never written to a plaintext file.
pub fn db_key() -> eyre::Result<[u8; 32]> {
    let entry = keyring::Entry::new(SERVICE, DB_KEY_USER).map_err(|err| {
        eyre::eyre!("failed to open Keychain entry {SERVICE}/{DB_KEY_USER}: {err}")
    })?;

    match entry.get_password() {
        Ok(hex) => decode_key(&hex),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            rand::fill(&mut key);
            entry
                .set_password(&hex_encode(&key))
                .map_err(|err| eyre::eyre!("failed to store db key in Keychain: {err}"))?;
            Ok(key)
        }
        Err(err) => Err(eyre::eyre!(
            "failed to read db key from Keychain entry {SERVICE}/{DB_KEY_USER}: {err}"
        )),
    }
}

/// `~/.local/share/telegram-tui/td/` (spec §9.3), created mode `0700` if it
/// doesn't already exist. An existing directory's mode is left untouched.
pub fn td_database_dir() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the data directory: {err}"))?;
    let dir = strategy.data_dir().join(APP_DIR).join(TD_SUBDIR);

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;

    Ok(dir)
}

fn decode_key(hex: &str) -> eyre::Result<[u8; 32]> {
    let bytes = hex_decode(hex).ok_or_else(|| {
        eyre::eyre!("Keychain entry {SERVICE}/{DB_KEY_USER} is not valid hex (corrupted?)")
    })?;
    let len = bytes.len();
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        eyre::eyre!(
            "Keychain entry {SERVICE}/{DB_KEY_USER} decodes to {len} bytes, expected 32 (corrupted?)"
        )
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn hex_roundtrips() {
        let key = [7u8; 32];
        let encoded = hex_encode(&key);
        assert_eq!(encoded.len(), 64);
        assert_eq!(decode_key(&encoded).unwrap(), key);
    }

    #[test]
    fn decode_key_rejects_non_hex() {
        assert!(decode_key("not hex, obviously").is_err());
    }

    #[test]
    fn decode_key_rejects_wrong_length() {
        // Valid hex, but only 4 bytes worth.
        assert!(decode_key("deadbeef").is_err());
    }

    #[test]
    fn td_database_dir_created_with_mode_0700() {
        let _lock = lock_env();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK; no other thread in this test
        // binary reads or writes XDG_DATA_HOME while this guard is held.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }

        let dir = td_database_dir().expect("should create the td database dir");
        assert!(dir.starts_with(tmp.path()));
        assert!(dir.ends_with("telegram-tui/td"));

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected mode 0700, got {mode:o}");

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
    }

    /// Touches the real macOS Keychain, which may prompt for permission the
    /// first time it runs interactively. Run manually with:
    /// `cargo test -p tgt-app keychain -- --ignored`.
    #[test]
    #[ignore]
    fn db_key_is_stable_across_calls() {
        let first = db_key().expect("db_key should succeed against the real Keychain");
        let second = db_key().expect("db_key should succeed on a second call");
        assert_eq!(
            first, second,
            "db_key should return the same key once stored"
        );
    }
}
