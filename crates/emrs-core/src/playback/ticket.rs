//! 短票据播放：jwt 自校验，用于 `/s/{ticket}` 端点。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 票据载荷。
#[derive(Debug, Serialize, Deserialize)]
pub struct TicketClaims {
    /// 媒体 UUID
    pub uuid: String,
    /// 用户 ID（0 表示匿名）
    pub user_id: i64,
    /// 过期时间戳（秒）
    pub exp: u64,
}

/// 签发票据。
pub fn issue_ticket(claims: &TicketClaims, secret: &[u8]) -> Result<String> {
    use jsonwebtoken::{EncodingKey, Header, encode};
    let token = encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(secret),
    )?;
    Ok(token)
}

/// 验证票据。
pub fn verify_ticket(token: &str, secret: &[u8]) -> Result<TicketClaims> {
    use jsonwebtoken::{DecodingKey, Validation, decode};
    let data = decode::<TicketClaims>(
        token,
        &DecodingKey::from_secret(secret),
        &Validation::default(),
    )
    .context("票据验证失败")?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let claims = TicketClaims {
            uuid: "abc-123".into(),
            user_id: 42,
            exp: 9999999999,
        };
        let token = issue_ticket(&claims, b"secret-key").unwrap();
        let verified = verify_ticket(&token, b"secret-key").unwrap();
        assert_eq!(verified.uuid, "abc-123");
        assert_eq!(verified.user_id, 42);
    }

    #[test]
    fn wrong_secret_fails() {
        let claims = TicketClaims {
            uuid: "abc".into(),
            user_id: 1,
            exp: 9999999999,
        };
        let token = issue_ticket(&claims, b"key-a").unwrap();
        assert!(verify_ticket(&token, b"key-b").is_err());
    }
}
