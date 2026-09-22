use crate::util::hash::md5_hex;
use rand::{distributions::Alphanumeric, Rng};

pub fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

pub fn gen_encrypted_password(password: &str, salt: &str) -> String {
    let first = md5_hex(&format!("{}{}", password, salt));
    md5_hex(&format!("{}{}", first, salt))
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
}
