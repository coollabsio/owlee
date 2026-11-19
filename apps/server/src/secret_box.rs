//! Secrets at rest: ChaCha20-Poly1305 under `OWLEE_APP_KEY`. A stored value is
//! `v1:` + base64(nonce || ciphertext). The associated data names the column,
//! so a sealed value cannot be moved to another one.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use owlee_platform::AppKey;

const PREFIX: &str = "v1:";

pub(crate) fn seal(key: &AppKey, aad: &[u8], plaintext: &str) -> String {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key.bytes()));
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad,
            },
        )
        .expect("ChaCha20-Poly1305 encrypts any length a TEXT column holds");
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&encrypted);
    format!("{PREFIX}{}", STANDARD.encode(sealed))
}

/// `None` for another key, other associated data, altered bytes, or a value
/// that was never sealed.
pub(crate) fn open(key: &AppKey, aad: &[u8], stored: &str) -> Option<String> {
    let sealed = STANDARD.decode(stored.strip_prefix(PREFIX)?).ok()?;
    if sealed.len() < 28 {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key.bytes()));
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&sealed[..12]),
            Payload {
                msg: &sealed[12..],
                aad,
            },
        )
        .ok()?;
    String::from_utf8(plaintext).ok()
}

#[cfg(test)]
mod tests {
    use owlee_platform::AppKey;

    use super::{open, seal};

    const KEY: AppKey = AppKey::new([7; 32]);

    #[test]
    fn opens_with_the_same_key_and_associated_data() {
        let sealed = seal(&KEY, b"column", "smtp password");
        assert!(sealed.starts_with("v1:"), "{sealed}");
        assert!(!sealed.contains("smtp password"));
        assert_eq!(
            open(&KEY, b"column", &sealed).as_deref(),
            Some("smtp password")
        );
    }

    #[test]
    fn does_not_open_with_another_key_column_or_plaintext() {
        let sealed = seal(&KEY, b"column", "secret");
        assert_eq!(open(&AppKey::new([8; 32]), b"column", &sealed), None);
        assert_eq!(open(&KEY, b"other", &sealed), None);
        assert_eq!(open(&KEY, b"column", &sealed[..sealed.len() - 4]), None);
        assert_eq!(open(&KEY, b"column", "secret"), None);
    }
}
