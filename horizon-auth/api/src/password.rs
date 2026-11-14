//! Password hashing with PBKDF2-HMAC-SHA256.
//!
//! A native mod may only depend on crates already in Horizon's `Cargo.lock`
//! (docs/native-mods.md, "Dependencies come from Horizon's Cargo.lock"), and the
//! launcher resolves them offline. `scrypt` is in the lock but its
//! `password-hash` dependency is not, so it cannot be used here. `pbkdf2`,
//! `hmac` and `sha2` are all present, so PBKDF2-HMAC-SHA256 is the KDF.
//!
//! Stored form (readable to an operator, not the `password-hash` crate's PHC
//! serialization):
//!
//! `pbkdf2$<iterations>$<salt base64>$<hash base64>`
//!
//! No plaintext password is ever written, logged or put in the database.
use crate::config::Config;
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const SALT_LEN: usize = 16;
pub const KEY_LEN: usize = 32;

/// `log_n` is `log2(iterations)`, so the stored iteration count stays an exact
/// power of two and the config field stays a small integer.
pub fn iterations_for(log_n: u8) -> u32 {
    1u32 << (log_n as u32)
}

pub fn hash(password: &str, log_n: u8) -> Result<String, String> {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|e| format!("no system randomness: {e}"))?;
    hash_with_salt(password, &salt, log_n)
}

pub fn hash_with_salt(password: &str, salt: &[u8], log_n: u8) -> Result<String, String> {
    if salt.is_empty() {
        return Err("empty salt".to_owned());
    }
    let rounds = iterations_for(log_n);
    let mut out = [0u8; KEY_LEN];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, rounds, &mut out);
    Ok(format!(
        "pbkdf2${rounds}${}${}",
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(out)
    ))
}

/// Constant-time check of a password against a stored hash. `Ok(false)` means
/// "wrong password"; `Err` means the stored value is malformed, which is a
/// storage problem rather than a failed login.
pub fn verify(password: &str, stored: &str) -> Result<bool, String> {
    let mut parts = stored.split('$');
    let algorithm = parts.next().ok_or("empty hash")?;
    if algorithm != "pbkdf2" {
        return Err(format!("unsupported password hash {algorithm:?}"));
    }
    let rounds: u32 = next(&mut parts)?.parse().map_err(|_| "bad iteration count")?;
    if rounds == 0 || rounds > 10_000_000 {
        return Err("iteration count out of range".to_owned());
    }
    let salt = STANDARD_NO_PAD
        .decode(next(&mut parts)?)
        .map_err(|_| "bad salt")?;
    let expected = STANDARD_NO_PAD
        .decode(next(&mut parts)?)
        .map_err(|_| "bad hash")?;
    if parts.next().is_some() {
        return Err("trailing data in password hash".to_owned());
    }
    if salt.is_empty() || expected.len() < 10 || expected.len() > 64 {
        return Err("bad salt or hash length".to_owned());
    }
    let mut actual = vec![0u8; expected.len()];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, rounds, &mut actual);
    Ok(bool::from(actual.ct_eq(&expected)))
}

fn next<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Result<&'a str, String> {
    parts.next().ok_or_else(|| "truncated password hash".to_owned())
}

/// The rule shown to players. Deliberately allows any non-whitespace,
/// non-control character; commands split at whitespace, so spaces cannot be
/// passwords.
pub fn validate(password: &str, name: &str, config: &Config) -> Result<(), String> {
    let len = password.chars().count();
    if len < config.min_password_length {
        return Err(format!(
            "Password must be at least {} characters.",
            config.min_password_length
        ));
    }
    if len > config.max_password_length {
        return Err(format!(
            "Password must be at most {} characters.",
            config.max_password_length
        ));
    }
    if password
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\u{a7}')
    {
        return Err("Password cannot contain spaces or control characters.".to_owned());
    }
    if password.eq_ignore_ascii_case(name) {
        return Err("Password cannot be your name.".to_owned());
    }
    let mut chars = password.chars();
    if let Some(first) = chars.next() {
        if chars.all(|c| c == first) {
            return Err("Password is too simple.".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_salt_is_not_reused() {
        let a = hash("correct horse battery staple", 15).unwrap();
        let b = hash("correct horse battery staple", 15).unwrap();
        assert_ne!(a, b, "each hash must carry a fresh salt");
        assert!(verify("correct horse battery staple", &a).unwrap());
        assert!(!verify("wrong password", &a).unwrap());
        assert!(verify("correct horse battery staple", &b).unwrap());
    }

    #[test]
    fn iteration_count_is_a_power_of_two() {
        assert_eq!(iterations_for(15), 32_768);
        assert_eq!(iterations_for(18), 262_144);
    }

    #[test]
    fn malformed_hashes_are_errors_not_logins() {
        assert!(verify("x", "not-a-hash").is_err());
        assert!(verify("x", "pbkdf2$0$@@@$@@@").is_err());
        assert!(verify("x", "pbkdf2$abc$@@@$@@@").is_err());
        assert!(verify("x", "scrypt$15$8$1$@@@$@@@").is_err());
        assert!(verify("x", "bcrypt$whatever").is_err());
    }

    #[test]
    fn validation_rules() {
        let config = Config::default();
        assert!(validate("short", "Steve", &config).is_err());
        assert!(validate("has space", "Steve", &config).is_err());
        assert!(validate("steve", "Steve", &config).is_err());
        assert!(validate("aaaaaaaa", "Steve", &config).is_err());
        assert!(validate("good pass", "Steve", &config).is_err());
        assert!(validate("good-password", "Steve", &config).is_ok());
    }
}
