use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::{distributions::Alphanumeric, Rng};

pub fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// Argon2id 密码哈希，输出自含参数与随机盐的 PHC 字符串（每次调用结果不同）。
///
/// 此前是 md5(md5(pwd+salt)+salt) 双轮 MD5：无记忆硬度、可 GPU 高速爆破，
/// 数据库泄露后口令可被离线枚举。
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

/// 校验口令与 PHC 哈希是否匹配；stored 不是合法 PHC 时一律视为不匹配。
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 常量时间字符串比较，用于邀请码这类共享密钥的校验，
/// 避免 `==` 的逐字节短路泄露前缀匹配信息。
pub fn secure_compare(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // 长度本身不是秘密（密钥长度由部署方决定），但内容比较必须全程无分支
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_string_has_expected_length_and_alphabet() {
        let value = random_string(32);
        assert_eq!(value.chars().count(), 32);
        assert!(value.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn random_string_does_not_repeat() {
        // 会话 token 的随机输入源必须每次都不同
        assert_ne!(random_string(32), random_string(32));
    }

    #[test]
    fn hash_password_produces_self_contained_phc() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        // 随机盐：同口令两次哈希结果不同
        let again = hash_password("correct horse battery staple").unwrap();
        assert_ne!(hash, again);
    }

    #[test]
    fn verify_password_accepts_correct_and_rejects_wrong() {
        let hash = hash_password("passw0rd!").unwrap();
        assert!(verify_password("passw0rd!", &hash));
        assert!(!verify_password("passw0rd?", &hash));
        assert!(!verify_password("", &hash));
    }

    #[test]
    fn verify_password_rejects_malformed_stored_value() {
        assert!(!verify_password("whatever", "not-a-phc"));
        assert!(!verify_password("whatever", ""));
        // 旧格式的双轮 MD5 十六进制串不再被接受
        assert!(!verify_password(
            "password123",
            "9b3fd8f60f2b0f4b32b8f1f0ec27b2dd"
        ));
    }

    #[test]
    fn secure_compare_matches_exactly() {
        assert!(secure_compare("s3cret-key", "s3cret-key"));
        assert!(!secure_compare("s3cret-key", "s3cret-keh"));
        assert!(!secure_compare("s3cret-key", "s3cret"));
        assert!(!secure_compare("", "x"));
        assert!(secure_compare("", ""));
    }
}
