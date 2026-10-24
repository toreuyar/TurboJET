//! HMAC authentication and replay window for external delivery callbacks.
//!
//! Canonical string: `timestamp + "\n" + event_id + "\n" + sha256_hex(body)`.
//! `event_id` is also the durable nonce: the database accepts it at most once
//! per release stream and rejects a different payload under the same id. A
//! timestamp outside the configured window is rejected before any DB write.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

pub const CALLBACK_TIMESTAMP_HEADER: &str = "x-turbojet-delivery-timestamp";
pub const CALLBACK_EVENT_HEADER: &str = "x-turbojet-delivery-event";
pub const CALLBACK_SIGNATURE_HEADER: &str = "x-turbojet-delivery-signature";
pub const CALLBACK_SOURCE_HEADER: &str = "x-turbojet-delivery-source";

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CallbackAuthError {
    #[error("callback timestamp is missing or invalid")]
    InvalidTimestamp,
    #[error("callback timestamp is outside the permitted replay window")]
    OutsideReplayWindow,
    #[error("callback event id is missing")]
    MissingEventId,
    #[error("callback signature is missing or malformed")]
    InvalidSignature,
    #[error("callback authentication is not configured")]
    NotConfigured,
}

pub fn canonical_payload(timestamp: &str, event_id: &str, body: &[u8]) -> String {
    format!(
        "{timestamp}\n{event_id}\n{}",
        hex::encode(Sha256::digest(body))
    )
}

pub fn sign_callback(secret: &[u8], timestamp: &str, event_id: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(canonical_payload(timestamp, event_id, body).as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, CallbackAuthError> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    let seconds = value
        .parse::<i64>()
        .map_err(|_| CallbackAuthError::InvalidTimestamp)?;
    DateTime::<Utc>::from_timestamp(seconds, 0).ok_or(CallbackAuthError::InvalidTimestamp)
}

pub fn verify_callback(
    secret: &[u8],
    timestamp: &str,
    event_id: &str,
    signature: &str,
    body: &[u8],
    now: DateTime<Utc>,
    window_secs: i64,
) -> Result<(), CallbackAuthError> {
    if event_id.trim().is_empty() {
        return Err(CallbackAuthError::MissingEventId);
    }
    let occurred = parse_timestamp(timestamp)?;
    let window = window_secs.max(1);
    if (now - occurred).num_seconds().abs() > window {
        return Err(CallbackAuthError::OutsideReplayWindow);
    }
    let signature = signature.strip_prefix("sha256=").unwrap_or(signature);
    let provided = hex::decode(signature).map_err(|_| CallbackAuthError::InvalidSignature)?;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| CallbackAuthError::NotConfigured)?;
    mac.update(canonical_payload(timestamp, event_id, body).as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| CallbackAuthError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn signature_is_bound_to_timestamp_event_and_body() {
        let secret = b"local-disposable-callback-secret";
        let timestamp = "2026-10-22T12:00:00Z";
        let now = DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Utc);
        let body = br#"{"event_id":"e1","sequence":1,"type":"ci","status":"succeeded"}"#;
        let signature = sign_callback(secret, timestamp, "e1", body);
        verify_callback(secret, timestamp, "e1", &signature, body, now, 300).unwrap();
        assert!(verify_callback(secret, timestamp, "e2", &signature, body, now, 300).is_err());
        assert!(verify_callback(secret, timestamp, "e1", &signature, br#"{}"#, now, 300).is_err());
        assert!(
            verify_callback(b"other-secret", timestamp, "e1", &signature, body, now, 300).is_err()
        );
    }

    #[test]
    fn stale_callbacks_and_missing_event_ids_are_rejected() {
        let secret = b"secret";
        let timestamp = "2026-10-22T12:00:00Z";
        let now = DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Utc)
            + TimeDelta::seconds(301);
        let body = b"{}";
        let signature = sign_callback(secret, timestamp, "e1", body);
        assert_eq!(
            verify_callback(secret, timestamp, "e1", &signature, body, now, 300).unwrap_err(),
            CallbackAuthError::OutsideReplayWindow
        );
        assert_eq!(
            verify_callback(secret, timestamp, "", &signature, body, now, 300).unwrap_err(),
            CallbackAuthError::MissingEventId
        );
    }

    #[test]
    fn unix_seconds_timestamp_is_accepted_with_equal_window() {
        let secret = b"secret";
        let body = b"{}";
        let timestamp = "1761134400";
        let now = DateTime::from_timestamp(1761134400, 0).unwrap();
        let signature = sign_callback(secret, timestamp, "e1", body);
        verify_callback(secret, timestamp, "e1", &signature, body, now, 1).unwrap();
    }
}
