//! 密码哈希（bcrypt）。

use anyhow::Result;

/// bcrypt 哈希；空密码返回空串（语义：空 hash 表示未设密码）。
pub fn hash_password(plaintext: &str) -> Result<String> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    Ok(bcrypt::hash(plaintext, bcrypt::DEFAULT_COST)?)
}

/// 校验 bcrypt；空 hash 视为"未设密码"恒通过。
pub fn verify_password(hash: &str, plaintext: &str) -> bool {
    if hash.trim().is_empty() {
        return true;
    }
    bcrypt::verify(plaintext, hash).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_verify_roundtrip() {
        let h = hash_password("s3cret").unwrap();
        assert!(h.starts_with("$2"));
        assert!(verify_password(&h, "s3cret"));
        assert!(!verify_password(&h, "wrong"));
        assert!(verify_password("", "anything"));
    }
}
