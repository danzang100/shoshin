//! Salted hashing and path normalization. Nothing raw is ever stored (spec 003 section 6.1).
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct Hasher {
    salt: String,
}

impl Hasher {
    pub fn new(salt: impl Into<String>) -> Self {
        Self { salt: salt.into() }
    }

    pub fn random_salt() -> String {
        format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
    }

    /// 16 hex chars of SHA-256(salt || 0x00 || data).
    pub fn hash(&self, data: &str) -> String {
        self.hash_bytes(data.as_bytes())
    }

    pub fn hash_bytes(&self, data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(self.salt.as_bytes());
        h.update([0u8]);
        h.update(data);
        hex16(&h.finalize())
    }

    pub fn hash_json(&self, v: &Value) -> String {
        self.hash(&canonical_json(v))
    }

    pub fn hash_path(&self, path: &str, cwd: Option<&str>) -> String {
        self.hash(&normalize_path(path, cwd))
    }
}

fn hex16(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// JSON with object keys sorted recursively, independent of serde_json feature flags.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&m[k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Forward slashes, lowercase drive letter, relative to `cwd` when under it.
pub fn normalize_path(path: &str, cwd: Option<&str>) -> String {
    let p = norm(path);
    if let Some(c) = cwd {
        let c = norm(c);
        let c = c.trim_end_matches('/');
        if !c.is_empty() {
            if p == c {
                return ".".to_string();
            }
            let prefix = format!("{c}/");
            if p.starts_with(&prefix) {
                return p[prefix.len()..].to_string();
            }
        }
    }
    p
}

fn is_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn norm(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    if is_drive(&s) {
        let first = s[..1].to_lowercase();
        s.replace_range(..1, &first);
    }
    s
}

/// Last path component, for display.
pub fn last_component(path: &str) -> String {
    let n = norm(path);
    n.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hash_is_salted_and_stable() {
        let a = Hasher::new("s1");
        let b = Hasher::new("s2");
        assert_eq!(a.hash("x"), a.hash("x"));
        assert_ne!(a.hash("x"), b.hash("x"));
        assert_eq!(a.hash("x").len(), 16);
    }

    #[test]
    fn canonical_json_ignores_key_order() {
        let h = Hasher::new("s");
        let x = json!({"b": 1, "a": {"d": [1, 2], "c": null}});
        let y: Value = serde_json::from_str(r#"{"a":{"c":null,"d":[1,2]},"b":1}"#).unwrap();
        assert_eq!(h.hash_json(&x), h.hash_json(&y));
        assert_ne!(h.hash_json(&x), h.hash_json(&json!({"b": 2, "a": {"d": [1, 2], "c": null}})));
    }

    #[test]
    fn paths_normalize() {
        assert_eq!(normalize_path(r"C:\Users\d\proj\src\a.rs", Some(r"c:\Users\d\proj")), "src/a.rs");
        assert_eq!(normalize_path(r"C:\Users\d\proj", Some(r"C:\Users\d\proj\")), ".");
        assert_eq!(normalize_path(r"C:\Other\a.rs", Some(r"C:\Users\d\proj")), "c:/Other/a.rs");
        assert_eq!(normalize_path("/home/d/p/a.rs", Some("/home/d/p")), "a.rs");
        assert_eq!(normalize_path("/home/d/pp/a.rs", Some("/home/d/p")), "/home/d/pp/a.rs");
        assert_eq!(normalize_path("rel/a.rs", None), "rel/a.rs");
    }

    #[test]
    fn same_target_hashes_equal_across_spellings() {
        let h = Hasher::new("s");
        assert_eq!(
            h.hash_path(r"C:\p\src\a.rs", Some(r"C:\p")),
            h.hash_path("c:/p/src/a.rs", Some("c:/p"))
        );
    }

    #[test]
    fn last_component_works() {
        assert_eq!(last_component(r"C:\Users\d\lightning\"), "lightning");
    }
}
