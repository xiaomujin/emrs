//! 随机 token 与哈希（sha256）。

use rand::RngCore;
use sha2::{Digest, Sha256};

/// url-safe hex token，长度 2*bytes。
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// sha256 hex（auth_token.token_hash）。
pub fn token_hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// token 前缀（前 8 位，用于人工辨识，不参与校验）。
pub fn token_prefix(token: &str) -> &str {
    if token.len() >= 8 { &token[..8] } else { token }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_token_shape() {
        let t = random_token(16);
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, random_token(16));
    }

    #[test]
    fn token_hash_is_sha256_hex() {
        let h = token_hash("abc");
        assert_eq!(h.len(), 64);
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn token_prefix_works() {
        assert_eq!(token_prefix("abcdef123456"), "abcdef12");
        assert_eq!(token_prefix("short"), "short");
    }
}
