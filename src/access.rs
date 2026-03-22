use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{Rng, RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime};

pub const GENERATE_PIN_SENTINEL: &str = "__beam_generate_pin__";

#[derive(Clone, Debug)]
pub struct AccessPolicy {
    pub expires_at: SystemTime,
    pub once: bool,
    pub pin_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AccessSetup {
    pub policy: AccessPolicy,
    pub revealed_pin: Option<String>,
}

pub fn build_access_policy(ttl: Duration, once: bool, pin_input: Option<String>) -> AccessSetup {
    let revealed_pin = pin_input.map(|value| {
        if value == GENERATE_PIN_SENTINEL {
            generate_pin()
        } else {
            value
        }
    });

    let policy = AccessPolicy {
        expires_at: SystemTime::now() + ttl,
        once,
        pin_hash: revealed_pin.as_deref().map(hash_pin),
    };

    AccessSetup {
        policy,
        revealed_pin,
    }
}

pub fn generate_token() -> String {
    let mut bytes = [0_u8; 24];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_pin() -> String {
    let value = OsRng.gen_range(0..1_000_000);
    format!("{value:06}")
}

pub fn verify_pin(expected_hash: Option<&str>, supplied_pin: Option<&str>) -> bool {
    match expected_hash {
        Some(hash) => supplied_pin.map(hash_pin).as_deref() == Some(hash),
        None => true,
    }
}

pub fn hash_pin(pin: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pin.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::{build_access_policy, generate_token, hash_pin, verify_pin};
    use std::time::Duration;

    #[test]
    fn generates_url_safe_tokens() {
        let token = generate_token();
        assert!(token.len() >= 32);
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
    }

    #[test]
    fn hashes_and_verifies_pin() {
        let hash = hash_pin("123456");
        assert!(verify_pin(Some(&hash), Some("123456")));
        assert!(!verify_pin(Some(&hash), Some("111111")));
        assert!(!verify_pin(Some(&hash), None));
        assert!(verify_pin(None, None));
    }

    #[test]
    fn builds_policy_with_explicit_pin() {
        let setup = build_access_policy(Duration::from_secs(60), true, Some("654321".to_string()));
        assert!(setup.policy.once);
        assert_eq!(setup.revealed_pin.as_deref(), Some("654321"));
        assert!(setup.policy.pin_hash.is_some());
    }
}
