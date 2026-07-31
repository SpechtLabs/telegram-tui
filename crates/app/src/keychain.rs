//! TDLib database encryption key, stored in the platform credential store via
//! the `keyring` crate — the Keychain on macOS, the Credential Manager on
//! Windows, and a D-Bus Secret Service provider on other unixes; generated on
//! first run and never written to disk in plaintext (spec §9.3). Also the
//! TDLib database directory itself.

use std::path::PathBuf;

use color_eyre::eyre::{self, Context};
use etcetera::BaseStrategy;

const SERVICE: &str = "telegram-tui";
const DB_KEY_USER: &str = "db-encryption-key";
const APP_DIR: &str = "telegram-tui";
const TD_SUBDIR: &str = "td";

/// Gets the 32-byte TDLib database encryption key from the platform
/// credential store, generating and storing a fresh random one on first run.
/// The key is stored hex-encoded (credential-store entries are UTF-8
/// strings); it is never held anywhere else, and never written to a plaintext
/// file.
///
/// On macOS and Windows the backing store is always there. On other unixes it
/// is a D-Bus Secret Service provider — gnome-keyring, KWallet, KeePassXC —
/// which a headless or bare ssh session may simply not have running, and
/// there is no fallback: without a store there is nowhere to keep the key
/// that isn't a plaintext file on disk, so this fails and startup stops.
pub fn db_key() -> eyre::Result<[u8; 32]> {
    let entry = keyring::Entry::new(SERVICE, DB_KEY_USER).map_err(|err| {
        eyre::eyre!("failed to open credential store entry {SERVICE}/{DB_KEY_USER}: {err}")
    })?;

    match entry.get_password() {
        Ok(hex) => decode_key(&hex),
        Err(keyring::Error::NoEntry) => {
            let mut key = [0u8; 32];
            rand::fill(&mut key);
            entry.set_password(&hex_encode(&key)).map_err(|err| {
                eyre::eyre!("failed to store db key in the credential store: {err}")
            })?;
            Ok(key)
        }
        Err(err) => Err(eyre::eyre!(
            "failed to read db key from credential store entry {SERVICE}/{DB_KEY_USER}: {err}"
        )),
    }
}

/// `~/.local/share/telegram-tui/td/` (spec §9.3), or
/// `%APPDATA%\telegram-tui\td\` on Windows. Created mode `0700` on unix if it
/// doesn't already exist; an existing directory's mode is left untouched.
///
/// That `0700` is a real privacy property rather than tidiness: this
/// directory holds the TDLib database, which is every message this client has
/// cached. Windows has no mode bits, so the directory is created with
/// whatever ACL it inherits from `%APPDATA%` — restrictive in practice on a
/// normal single-user profile, but inherited rather than asserted. Matching
/// the unix guarantee would take an explicit DACL, and that is unfinished
/// work, not parity.
pub fn td_database_dir() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the data directory: {err}"))?;
    let dir = strategy.data_dir().join(APP_DIR).join(TD_SUBDIR);

    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
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
    use super::*;

    // Only the unix-gated directory test below mutates the environment, so
    // the lock it needs is gated with it; leaving it visible everywhere would
    // be dead code under `-D warnings` on Windows.
    #[cfg(unix)]
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
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

    /// Unix only: the mode-bit assertion below has no Windows equivalent.
    /// `set_data_dir` (see its docs in `logging::tests`) does redirect this
    /// path on Windows too — via `LOCALAPPDATA`, since `etcetera`'s Windows
    /// strategy never consults `XDG_DATA_HOME` — but there are no permission
    /// bits there to check. Whether Windows creates the directory at all is
    /// left to the integration tests.
    #[cfg(unix)]
    #[test]
    fn td_database_dir_created_with_mode_0700() {
        use crate::logging::tests::{set_data_dir, unset_data_dir};

        let _lock = lock_env();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK; no other thread in this test
        // binary reads or writes the data-dir override while this guard is
        // held.
        unsafe {
            set_data_dir(tmp.path());
        }

        let dir = td_database_dir().expect("should create the td database dir");
        assert!(dir.starts_with(tmp.path()));
        assert!(dir.ends_with("telegram-tui/td"));

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "expected mode 0700, got {mode:o}");

        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            unset_data_dir();
        }
    }

    /// Touches the real platform credential store, which may prompt for
    /// permission the first time it runs interactively and needs a Secret
    /// Service provider to be running on non-Apple unixes. Run manually with:
    /// `cargo test -p tgt-app keychain -- --ignored`.
    #[test]
    #[ignore]
    fn db_key_is_stable_across_calls() {
        let first = db_key().expect("db_key should succeed against the real credential store");
        let second = db_key().expect("db_key should succeed on a second call");
        assert_eq!(
            first, second,
            "db_key should return the same key once stored"
        );
    }
}
