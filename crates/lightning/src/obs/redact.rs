//! Central secret redaction, applied before any sink sees an event (spec 001, section 9.4).
use serde_json::Value;

use super::event::Event;

const MASK: &str = "[REDACTED]";
/// Shorter secrets would mangle ordinary text, so they are ignored.
const MIN_SECRET_LEN: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = secrets.into_iter().filter(|s| s.len() >= MIN_SECRET_LEN).collect();
        // Longest first so a secret that contains another is masked whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Redactor { secrets }
    }

    /// Secrets from the usual environment variables (currently the Gemini key).
    pub fn from_env() -> Self {
        Redactor::new(["GEMINI_API_KEY"].iter().filter_map(|k| std::env::var(k).ok()))
    }

    pub fn redact_str(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), MASK);
            }
        }
        out
    }

    pub fn redact_value(&self, value: &mut Value) {
        match value {
            Value::String(s) => *s = self.redact_str(s),
            Value::Array(items) => items.iter_mut().for_each(|v| self.redact_value(v)),
            Value::Object(map) => map.values_mut().for_each(|v| self.redact_value(v)),
            _ => {}
        }
    }

    pub fn redact_event(&self, event: &Event) -> Event {
        if self.secrets.is_empty() {
            return event.clone();
        }
        let Ok(mut value) = serde_json::to_value(event) else {
            return event.clone();
        };
        self.redact_value(&mut value);
        serde_json::from_value(value).unwrap_or_else(|_| event.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obs::event::{EventKind, SCHEMA_VERSION};
    use serde_json::json;

    #[test]
    fn masks_secrets_in_nested_values() {
        let r = Redactor::new(["sk-secret-123456".to_string()]);
        let mut v = json!({ "a": "key=sk-secret-123456", "b": ["x", { "c": "sk-secret-123456" }], "n": 1 });
        r.redact_value(&mut v);
        assert_eq!(v, json!({ "a": "key=[REDACTED]", "b": ["x", { "c": "[REDACTED]" }], "n": 1 }));
    }

    #[test]
    fn ignores_short_secrets() {
        let r = Redactor::new(["abc".to_string()]);
        assert_eq!(r.redact_str("abc abc"), "abc abc");
    }

    #[test]
    fn redacts_inside_typed_events() {
        let r = Redactor::new(["sk-secret-123456".to_string()]);
        let e = Event {
            v: SCHEMA_VERSION,
            ts: "t".into(),
            session_id: "s".into(),
            seq: 1,
            kind: EventKind::UserMessage { text: "my key is sk-secret-123456".into() },
            span_id: None,
            parent_span_id: None,
        };
        let out = r.redact_event(&e);
        assert_eq!(out.kind, EventKind::UserMessage { text: "my key is [REDACTED]".into() });
        assert_eq!(out.seq, 1);
    }
}
