//! Credential-safe durable activity and support exports (TJ-024).
//!
//! Durable model/tool activity is inspectable by authorized project members,
//! but it must never persist bearer tokens, API keys, passwords, cookies or
//! private-key blocks. Redaction happens before the row is written, and again
//! on export so older/pre-existing rows are also protected.

use serde_json::Value;

const MASK: &str = "[REDACTED]";

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("password")
        || key.contains("secret")
        || key.contains("token")
        || key.contains("authorization")
        || key.contains("cookie")
        || key == "key"
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_sensitive_key(key) && !child.is_null() {
                    *child = Value::String(MASK.into());
                } else {
                    redact_json(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json(item);
            }
        }
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(mut nested) = serde_json::from_str::<Value>(trimmed) {
                    redact_json(&mut nested);
                    if let Ok(serialized) = serde_json::to_string(&nested) {
                        *text = serialized;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Mask private-key blocks and common credential shapes in free text.
pub fn redact_sensitive(input: &str) -> String {
    let mut out = redact_private_keys(input);
    out = redact_header_values(&out, "authorization");
    out = redact_header_values(&out, "cookie");
    out = redact_prefixed_tokens(&out);
    out = redact_json_credentials(&out);
    out
}

fn redact_private_keys(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("-----BEGIN ") {
        let Some(header_end_rel) = rest[start..].find("PRIVATE KEY-----") else {
            break;
        };
        let header_end = start + header_end_rel + "PRIVATE KEY-----".len();
        let body = &rest[header_end..];
        let Some(end_rel) = body.find("-----END ") else {
            break;
        };
        let end = header_end + end_rel;
        let Some(close_rel) = rest[end..].find("-----") else {
            break;
        };
        let close = end + close_rel + 5;
        out.push_str(&rest[..start]);
        out.push_str(MASK);
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

fn redact_header_values(input: &str, name: &str) -> String {
    if !input.to_ascii_lowercase().contains(name) {
        return input.to_string();
    }
    input
        .split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            let lower = trimmed.to_ascii_lowercase();
            // Only actual header lines, never a JSON key embedded in a line.
            if lower.starts_with(name) && lower[name.len()..].trim_start().starts_with(':') {
                let colon = trimmed.find(':').unwrap_or(0);
                let indent = &line[..line.len() - trimmed.len()];
                return format!("{indent}{}{} {}\n", &trimmed[..colon], ':', MASK);
            }
            line.to_string()
        })
        .collect()
}

fn redact_prefixed_tokens(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for token in input.split_inclusive(char::is_whitespace) {
        let trimmed = token.trim_end();
        let lower = trimmed.to_ascii_lowercase();
        let sensitive = lower.starts_with("bearer ")
            || lower.starts_with("sk-")
            || lower.starts_with("sk_")
            || lower.starts_with("ghp_")
            || lower.starts_with("gho_")
            || lower.starts_with("github_pat_")
            || lower.starts_with("xoxb-")
            || lower.starts_with("xoxp-")
            || lower.starts_with("aiza")
            || (lower.starts_with("key-") && trimmed.len() > 12);
        if sensitive {
            let suffix: String = token[trimmed.len()..].to_string();
            out.push_str(MASK);
            out.push_str(&suffix);
        } else {
            out.push_str(token);
        }
    }
    out
}

fn redact_json_credentials(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(mut value) = serde_json::from_str::<Value>(trimmed) {
            redact_json(&mut value);
            if let Ok(serialized) = serde_json::to_string(&value) {
                // Preserve the original leading/trailing whitespace.
                let start = input.find(trimmed).unwrap_or(0);
                let mut out = String::new();
                out.push_str(&input[..start]);
                out.push_str(&serialized);
                out.push_str(&input[start + trimmed.len()..]);
                return out;
            }
        }
    }
    // Log lines may prefix a JSON payload. Redact that embedded object/array
    // without rewriting the surrounding text.
    if let (Some(start), Some(end)) = (input.find('{'), input.rfind('}'))
        && start < end
        && let Ok(mut value) = serde_json::from_str::<Value>(&input[start..=end])
    {
        redact_json(&mut value);
        if let Ok(serialized) = serde_json::to_string(&value) {
            return format!("{}{}{}", &input[..start], serialized, &input[end + 1..]);
        }
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_tokens_headers_and_json_credentials() {
        let text = "Authorization: Bearer sk-live-123456789\ncookie: session=abc\n\
                    token sk-proj-abcdefghijkl ghp_0123456789abcdefghij\n\
                    {\"api_key\":\"secret-value\",\"nested\":{\"password\":\"pw\"},\"ok\":\"visible\"}";
        let redacted = redact_sensitive(text);
        for secret in [
            "sk-live-123456789",
            "session=abc",
            "sk-proj-abcdefghijkl",
            "ghp_0123456789abcdefghij",
            "secret-value",
            "\"pw\"",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("visible"));
    }

    #[test]
    fn masks_private_key_blocks() {
        let text = "before -----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY----- after";
        let redacted = redact_sensitive(text);
        assert!(!redacted.contains("AAAA"));
        assert!(redacted.contains("before"));
        assert!(redacted.contains("after"));
    }

    #[test]
    fn masks_credentials_inside_nested_payload_strings() {
        let payload = serde_json::json!({
            "event": "tool_result",
            "payload": "{\"api_key\":\"nested-secret\",\"Authorization\":\"Bearer deep-token\",\"ok\":true}"
        });
        let redacted = redact_sensitive(&payload.to_string());
        assert!(!redacted.contains("nested-secret"));
        assert!(!redacted.contains("deep-token"));
        assert!(redacted.contains("tool_result"));
    }

    #[test]
    fn leaves_ordinary_prompt_text_intact() {
        let text = "The user wants a health endpoint and a clear error message.";
        assert_eq!(redact_sensitive(text), text);
    }
}
