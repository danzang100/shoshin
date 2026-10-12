//! Versioned, user-overridable price table. Prices are never guessed.
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_TABLE: &str = include_str!("../pricing/default.toml");

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub id: String,
    /// "exact" (default) or "prefix".
    #[serde(rename = "match", default)]
    pub match_kind: Option<String>,
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_5m_per_mtok: Option<f64>,
    pub cache_write_1h_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTable {
    version: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    model: Vec<ModelPrice>,
}

#[derive(Debug, Clone)]
pub struct PriceTable {
    pub version: String,
    pub currency: String,
    models: Vec<ModelPrice>,
}

/// Token counts for one cost computation; `None` is treated as zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokens {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_write_5m: Option<i64>,
    pub cache_write_1h: Option<i64>,
}

impl PriceTable {
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawTable = toml::from_str(text).context("parse pricing table")?;
        Ok(Self {
            version: raw.version,
            currency: raw.currency.unwrap_or_else(|| "USD".into()),
            models: raw.model,
        })
    }

    pub fn builtin() -> Self {
        Self::parse(DEFAULT_TABLE).expect("shipped pricing table must parse")
    }

    /// Shipped table, with entries from `override_path` replacing same-id entries.
    /// The override's version wins. A missing override file is not an error.
    pub fn load(override_path: Option<&Path>) -> Result<Self> {
        let mut t = Self::builtin();
        if let Some(p) = override_path.filter(|p| p.is_file()) {
            let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
            let o = Self::parse(&text)?;
            t.version = o.version;
            t.currency = o.currency;
            for m in o.models {
                match t.models.iter_mut().find(|x| x.id == m.id) {
                    Some(slot) => *slot = m,
                    None => t.models.push(m),
                }
            }
        }
        Ok(t)
    }

    pub fn models(&self) -> &[ModelPrice] {
        &self.models
    }

    /// Exact match first, then the longest matching prefix entry.
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        self.models
            .iter()
            .find(|m| m.id == model && m.match_kind.as_deref() != Some("prefix"))
            .or_else(|| {
                self.models
                    .iter()
                    .filter(|m| m.match_kind.as_deref() == Some("prefix") && model.starts_with(&m.id))
                    .max_by_key(|m| m.id.len())
            })
    }

    /// Estimated cost, or `None` when the model is unknown or a price needed for a
    /// non-zero token class is missing. Never returns 0 for "don't know".
    pub fn cost(&self, model: &str, t: Tokens) -> Option<f64> {
        let p = self.lookup(model)?;
        let parts = [
            (t.input, p.input_per_mtok),
            (t.output, p.output_per_mtok),
            (t.cache_read, p.cache_read_per_mtok),
            (t.cache_write_5m, p.cache_write_5m_per_mtok),
            (t.cache_write_1h, p.cache_write_1h_per_mtok),
        ];
        let mut total = 0.0;
        let mut any_price = false;
        for (tokens, price) in parts {
            let n = tokens.unwrap_or(0);
            match price {
                Some(pr) => {
                    any_price = true;
                    total += n as f64 / 1e6 * pr;
                }
                None if n > 0 => return None,
                None => {}
            }
        }
        any_price.then_some(total)
    }

    /// Days between the table version (a YYYY-MM-DD date) and `today`; `None` if unparsable.
    pub fn age_days(&self, today: chrono::NaiveDate) -> Option<i64> {
        let d = chrono::NaiveDate::parse_from_str(&self.version, "%Y-%m-%d").ok()?;
        Some((today - d).num_days())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = r#"
version = "2026-01-01"
[[model]]
id = "fake-model"
input_per_mtok = 1.0
output_per_mtok = 10.0
cache_read_per_mtok = 0.1
cache_write_5m_per_mtok = 1.25
cache_write_1h_per_mtok = 2.0

[[model]]
id = "fake-"
match = "prefix"
input_per_mtok = 3.0
output_per_mtok = 4.0

[[model]]
id = "fake-long-"
match = "prefix"
input_per_mtok = 5.0
output_per_mtok = 6.0
"#;

    #[test]
    fn shipped_table_parses_and_has_no_prices() {
        let t = PriceTable::builtin();
        assert!(t.models().len() >= 4);
        assert!(t.models().iter().all(|m| m.input_per_mtok.is_none()));
        let tokens = Tokens { input: Some(1000), ..Default::default() };
        assert_eq!(t.cost("claude-opus-5", tokens), None);
    }

    #[test]
    fn cost_math_uses_each_token_class() {
        let t = PriceTable::parse(FAKE).unwrap();
        let c = t
            .cost(
                "fake-model",
                Tokens {
                    input: Some(1_000_000),
                    output: Some(500_000),
                    cache_read: Some(2_000_000),
                    cache_write_5m: Some(1_000_000),
                    cache_write_1h: Some(500_000),
                },
            )
            .unwrap();
        // 1.0 + 5.0 + 0.2 + 1.25 + 1.0
        assert!((c - 8.45).abs() < 1e-9, "{c}");
    }

    #[test]
    fn missing_price_for_used_class_is_unpriced_but_unused_class_is_fine() {
        let t = PriceTable::parse(FAKE).unwrap();
        // prefix entry lacks cache prices
        let used = Tokens { input: Some(1), cache_read: Some(5), ..Default::default() };
        assert_eq!(t.cost("fake-x", used), None);
        let unused = Tokens { input: Some(1_000_000), output: Some(1_000_000), cache_read: Some(0), ..Default::default() };
        assert!((t.cost("fake-x", unused).unwrap() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_and_zero_tokens_are_not_zero_dollars() {
        let t = PriceTable::parse(FAKE).unwrap();
        assert_eq!(t.cost("nope", Tokens { input: Some(10), ..Default::default() }), None);
        // A priced model with no tokens costs a real 0.0.
        assert_eq!(t.cost("fake-model", Tokens::default()), Some(0.0));
    }

    #[test]
    fn lookup_prefers_exact_then_longest_prefix() {
        let t = PriceTable::parse(FAKE).unwrap();
        assert_eq!(t.lookup("fake-model").unwrap().input_per_mtok, Some(1.0));
        assert_eq!(t.lookup("fake-zzz").unwrap().input_per_mtok, Some(3.0));
        assert_eq!(t.lookup("fake-long-1").unwrap().input_per_mtok, Some(5.0));
    }

    #[test]
    fn override_replaces_by_id_and_adds_new() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.toml");
        std::fs::write(
            &p,
            "version = \"2026-10-01\"\n[[model]]\nid = \"claude-opus-5\"\ninput_per_mtok = 2.0\noutput_per_mtok = 4.0\n[[model]]\nid = \"made-up\"\ninput_per_mtok = 1.0\n",
        )
        .unwrap();
        let t = PriceTable::load(Some(&p)).unwrap();
        assert_eq!(t.version, "2026-10-01");
        assert_eq!(t.lookup("claude-opus-5").unwrap().input_per_mtok, Some(2.0));
        assert!(t.lookup("made-up").is_some());
        assert!(t.lookup("claude-sonnet-5").is_some());
        let missing = PriceTable::load(Some(&dir.path().join("none.toml"))).unwrap();
        assert_eq!(missing.version, "2026-10-12");
    }

    #[test]
    fn age_in_days() {
        let t = PriceTable::parse(FAKE).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        assert_eq!(t.age_days(today), Some(90));
    }
}
