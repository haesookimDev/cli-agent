use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Result, anyhow};
use axum::http::HeaderMap;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(format!("{:02x}", byte).as_str());
    }
    out
}

pub fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> Result<String> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret)?;
    mac.update(message);
    Ok(to_hex(&mac.finalize().into_bytes()))
}

/// Verifies that a signed request is authentic, given the request headers and body.
/// Each implementation encapsulates the protocol-specific layout (header names, signing
/// basestring, key material, timestamp tolerance).
pub trait SignatureVerifier {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<()>;
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow!("missing {name}"))
}

/// Slack Events / Interactivity signature: HMAC-SHA256 over `v0:{ts}:{body}`,
/// compared against the `v0=<hex>` value in `X-Slack-Signature`.
/// Rejects requests whose `X-Slack-Request-Timestamp` drifts by more than
/// `max_drift_secs` from the current clock.
pub struct SlackHmacVerifier {
    pub signing_secret: String,
    pub max_drift_secs: i64,
}

impl SlackHmacVerifier {
    pub fn new(signing_secret: impl Into<String>) -> Self {
        Self {
            signing_secret: signing_secret.into(),
            max_drift_secs: 300,
        }
    }
}

impl SignatureVerifier for SlackHmacVerifier {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<()> {
        let timestamp = header_str(headers, "X-Slack-Request-Timestamp")?;
        let signature = header_str(headers, "X-Slack-Signature")?;

        let ts: i64 = timestamp
            .parse()
            .map_err(|_| anyhow!("invalid timestamp"))?;
        let now = chrono::Utc::now().timestamp();
        if (now - ts).abs() > self.max_drift_secs {
            return Err(anyhow!("timestamp too old"));
        }

        let basestring = format!("v0:{}:{}", timestamp, String::from_utf8_lossy(body));
        let expected = format!(
            "v0={}",
            hmac_sha256_hex(self.signing_secret.as_bytes(), basestring.as_bytes())?
        );

        if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
            return Err(anyhow!("signature mismatch"));
        }

        Ok(())
    }
}

/// Discord Interaction signature: Ed25519 over `{timestamp}{body}`, verified
/// against the application public key stored as a hex string.
pub struct DiscordEd25519Verifier {
    pub public_key_hex: String,
}

impl DiscordEd25519Verifier {
    pub fn new(public_key_hex: impl Into<String>) -> Self {
        Self {
            public_key_hex: public_key_hex.into(),
        }
    }
}

impl SignatureVerifier for DiscordEd25519Verifier {
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<()> {
        let signature_hex = header_str(headers, "X-Signature-Ed25519")?;
        let timestamp = header_str(headers, "X-Signature-Timestamp")?;

        let public_key_bytes =
            hex::decode(&self.public_key_hex).map_err(|_| anyhow!("invalid public key hex"))?;
        let verifying_key = VerifyingKey::from_bytes(
            public_key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("public key must be 32 bytes"))?,
        )
        .map_err(|_| anyhow!("invalid Ed25519 public key"))?;

        let signature_bytes =
            hex::decode(signature_hex).map_err(|_| anyhow!("invalid signature hex"))?;
        let signature = Signature::from_bytes(
            signature_bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("signature must be 64 bytes"))?,
        );

        let mut message = Vec::with_capacity(timestamp.len() + body.len());
        message.extend_from_slice(timestamp.as_bytes());
        message.extend_from_slice(body);

        verifying_key
            .verify(&message, &signature)
            .map_err(|_| anyhow!("Ed25519 signature verification failed"))
    }
}

/// Derive a 32-byte AES-256 key from an arbitrary passphrase. The passphrase
/// usually comes from `CLI_AGENT_DB_KEY` env var.
fn derive_key(passphrase: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(passphrase.as_bytes());
    let out = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    key
}

const SECRET_PREFIX: &str = "enc:v1:";

/// Encrypt `plaintext` with AES-256-GCM and return a self-describing string
/// of the form `enc:v1:<base64(nonce||ciphertext)>`. Decryption is paired
/// with `decrypt_secret`. Plaintext that is already encrypted is left as-is
/// so callers can call this idempotently during writes.
pub fn encrypt_secret(plaintext: &str, passphrase: &str) -> Result<String> {
    if plaintext.starts_with(SECRET_PREFIX) {
        return Ok(plaintext.to_string());
    }
    let key = derive_key(passphrase);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow!("aes key init failed: {e}"))?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("aes encrypt failed: {e}"))?;
    let mut payload = Vec::with_capacity(12 + ciphertext.len());
    payload.extend_from_slice(&nonce_bytes);
    payload.extend_from_slice(&ciphertext);
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    Ok(format!("{SECRET_PREFIX}{encoded}"))
}

/// Inverse of `encrypt_secret`. Strings without the `enc:v1:` prefix are
/// returned unchanged so legacy plaintext rows keep working until they are
/// rewritten on the next save.
pub fn decrypt_secret(stored: &str, passphrase: &str) -> Result<String> {
    let Some(b64) = stored.strip_prefix(SECRET_PREFIX) else {
        return Ok(stored.to_string());
    };
    let payload = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| anyhow!("base64 decode failed: {e}"))?;
    if payload.len() < 12 {
        return Err(anyhow!("encrypted payload shorter than nonce"));
    }
    let (nonce_bytes, ciphertext) = payload.split_at(12);
    let key = derive_key(passphrase);
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| anyhow!("aes key init failed: {e}"))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("aes decrypt failed: {e}"))?;
    Ok(String::from_utf8(plaintext)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_equal_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn hmac_sha256_hex_is_deterministic() {
        let a = hmac_sha256_hex(b"secret", b"hello").unwrap();
        let b = hmac_sha256_hex(b"secret", b"hello").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn slack_verifier_accepts_self_signed_request() {
        let verifier = SlackHmacVerifier::new("topsecret");
        let body = b"payload";
        let ts = chrono::Utc::now().timestamp().to_string();
        let basestring = format!("v0:{}:{}", ts, String::from_utf8_lossy(body));
        let sig = format!(
            "v0={}",
            hmac_sha256_hex(b"topsecret", basestring.as_bytes()).unwrap()
        );

        let mut headers = HeaderMap::new();
        headers.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        headers.insert("X-Slack-Signature", sig.parse().unwrap());

        verifier.verify(&headers, body).unwrap();
    }

    #[test]
    fn slack_verifier_rejects_stale_timestamp() {
        let verifier = SlackHmacVerifier::new("topsecret");
        let body = b"payload";
        let ts = (chrono::Utc::now().timestamp() - 10_000).to_string();
        let basestring = format!("v0:{}:{}", ts, String::from_utf8_lossy(body));
        let sig = format!(
            "v0={}",
            hmac_sha256_hex(b"topsecret", basestring.as_bytes()).unwrap()
        );

        let mut headers = HeaderMap::new();
        headers.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        headers.insert("X-Slack-Signature", sig.parse().unwrap());

        assert!(verifier.verify(&headers, body).is_err());
    }

    #[test]
    fn encrypt_then_decrypt_secret_roundtrips() {
        let pass = "topsecret";
        let plain = "my-very-secret-webhook-token";
        let stored = encrypt_secret(plain, pass).unwrap();
        assert!(stored.starts_with(SECRET_PREFIX));
        assert_ne!(stored, plain, "stored value must not be plaintext");
        let recovered = decrypt_secret(&stored, pass).unwrap();
        assert_eq!(recovered, plain);
    }

    #[test]
    fn encrypt_secret_is_idempotent_on_already_encrypted_input() {
        let pass = "topsecret";
        let encrypted = encrypt_secret("hello", pass).unwrap();
        let again = encrypt_secret(&encrypted, pass).unwrap();
        assert_eq!(encrypted, again);
    }

    #[test]
    fn decrypt_secret_passthrough_for_legacy_plaintext() {
        // Anything without enc:v1: prefix is returned as-is.
        let plain = "legacy-plain";
        let recovered = decrypt_secret(plain, "any").unwrap();
        assert_eq!(recovered, plain);
    }

    #[test]
    fn decrypt_secret_fails_with_wrong_passphrase() {
        let stored = encrypt_secret("hello", "right").unwrap();
        assert!(decrypt_secret(&stored, "wrong").is_err());
    }

    #[test]
    fn encrypt_secret_uses_fresh_nonce() {
        let pass = "topsecret";
        let a = encrypt_secret("payload", pass).unwrap();
        let b = encrypt_secret("payload", pass).unwrap();
        assert_ne!(a, b, "same plaintext must encrypt to different ciphertexts");
    }

    #[test]
    fn slack_verifier_rejects_tampered_signature() {
        let verifier = SlackHmacVerifier::new("topsecret");
        let body = b"payload";
        let ts = chrono::Utc::now().timestamp().to_string();

        let mut headers = HeaderMap::new();
        headers.insert("X-Slack-Request-Timestamp", ts.parse().unwrap());
        headers.insert("X-Slack-Signature", "v0=deadbeef".parse().unwrap());

        assert!(verifier.verify(&headers, body).is_err());
    }
}
