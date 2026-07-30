//! HMAC-SHA256(id, per-install salt), truncated to 8 bytes, lowercase hex.
//! Salt generated locally in tgt-app, never transmitted: stable within an
//! install, uncorrelatable across installs, irreversible.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 over the id's little-endian byte representation (byte order
/// is an arbitrary but fixed choice; it only needs to be stable for a given
/// id across calls, which `i64::to_le_bytes` guarantees).
pub fn hash_id(salt: &[u8; 32], id: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(salt).expect("HMAC-SHA256 accepts a key of any size");
    mac.update(&id.to_le_bytes());
    let digest = mac.finalize().into_bytes();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_id_is_stable_within_salt_and_differs_across_salts() {
        let salt_a = [1u8; 32];
        let salt_b = [2u8; 32];

        let first = hash_id(&salt_a, 12345);
        let second = hash_id(&salt_a, 12345);
        assert_eq!(first, second, "same salt + id must be stable");

        let differing_salt = hash_id(&salt_b, 12345);
        assert_ne!(
            first, differing_salt,
            "different salts must yield different hashes for the same id"
        );
    }

    #[test]
    fn hash_id_is_8_bytes_hex() {
        let hash = hash_id(&[0u8; 32], 987654321);
        assert_eq!(hash.len(), 16, "8 bytes -> 16 lowercase hex chars");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got {hash}"
        );
    }
}
