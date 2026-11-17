//! Price per token and context window per model, embedded as a snapshot.
//!
//! [`embedded`] is the table in `assets/pricing.json`, taken from Anthropic's
//! first-party price list on the date in [`Pricing::as_of`]. It prices what a
//! transcript reports — input, output, cache writes by lifetime, cache reads —
//! and knows two modifiers the transcripts can show: fast mode and Claude
//! Haiku 5.5's higher prices for long prompts. Partner platforms (Bedrock,
//! Vertex AI), batch discounts and data-residency multipliers are not
//! modelled, so a cost from this module is an estimate.
//!
//! An unknown model has no price and no window: [`Pricing::model`] returns
//! `None`, and a caller shows `—`, never `$0`.

use std::sync::OnceLock;

use serde::Deserialize;

const EMBEDDED: &str = include_str!("../assets/pricing.json");

/// The embedded snapshot.
pub fn embedded() -> &'static Pricing {
    static TABLE: OnceLock<Pricing> = OnceLock::new();
    TABLE.get_or_init(|| {
        Pricing::from_json(EMBEDDED).expect("assets/pricing.json is validated by the tests")
    })
}

/// A price table.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    as_of: String,
    source: String,
    unit: String,
    models: Vec<Model>,
}

impl Pricing {
    /// Parses and validates a table in the format of `assets/pricing.json`.
    pub fn from_json(json: &str) -> Result<Self, PricingError> {
        let table: Self = serde_json::from_str(json)?;
        table.validate()?;
        Ok(table)
    }

    /// The date the snapshot was taken (`YYYY-MM-DD`).
    pub fn as_of(&self) -> &str {
        &self.as_of
    }

    /// Where the prices were read from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Every model in the table.
    pub fn models(&self) -> &[Model] {
        &self.models
    }

    /// Looks a model up by the id a transcript carries. Dated ids
    /// (`claude-haiku-4-5-20251001`, `claude-opus-4-5@20251101`) and Claude
    /// Code's `[1m]` suffix resolve to the base model. `None` for a model the
    /// table does not know.
    pub fn model(&self, id: &str) -> Option<&Model> {
        let exact = |id: &str| self.models.iter().find(|m| m.id == id);
        exact(id).or_else(|| exact(base_id(id)))
    }

    fn validate(&self) -> Result<(), PricingError> {
        let invalid = |id: &str, what: &str| Err(PricingError::Invalid(format!("{id}: {what}")));
        for (i, model) in self.models.iter().enumerate() {
            let id = model.id.as_str();
            if id.is_empty() || base_id(id) != id {
                return invalid(id, "an id must be a base model id");
            }
            if self.models[..i].iter().any(|m| m.id == id) {
                return invalid(id, "listed twice");
            }
            if model.context_window == 0 {
                return invalid(id, "context_window must be positive");
            }
            if !model.rates.is_valid() {
                return invalid(id, "rates must be positive and finite");
            }
            if let Some(long) = &model.long_context {
                if long.over_tokens == 0 || !long.rates.is_valid() {
                    return invalid(id, "long_context is malformed");
                }
            }
            if model
                .fast_multiplier
                .is_some_and(|m| !(m.is_finite() && m >= 1.0))
            {
                return invalid(id, "fast_multiplier must be at least 1");
            }
        }
        Ok(())
    }
}

/// One model's prices and limits.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    /// The base model id (`claude-opus-5-5`).
    pub id: String,
    /// The largest context the model accepts, in tokens.
    pub context_window: u64,
    /// The standard prices.
    pub rates: Rates,
    /// Higher prices for requests whose prompt is long, if the model has them.
    #[serde(default)]
    pub long_context: Option<LongContext>,
    /// What fast mode multiplies every price by, if the model has a fast mode.
    #[serde(default)]
    pub fast_multiplier: Option<f64>,
}

impl Model {
    /// The estimated cost of one API response, in USD.
    ///
    /// `fast` is whether the response ran in fast mode (`usage.speed`). On a
    /// model without a fast mode it is ignored.
    pub fn cost(&self, tokens: &Tokens, fast: bool) -> f64 {
        let rates = match &self.long_context {
            Some(long) if tokens.prompt() > long.over_tokens => &long.rates,
            _ => &self.rates,
        };
        let per_million = tokens.input as f64 * rates.input
            + tokens.output as f64 * rates.output
            + tokens.cache_write_5m as f64 * rates.cache_write_5m
            + tokens.cache_write_1h as f64 * rates.cache_write_1h
            + tokens.cache_read as f64 * rates.cache_read;
        let multiplier = if fast {
            self.fast_multiplier.unwrap_or(1.0)
        } else {
            1.0
        };
        per_million * multiplier / 1_000_000.0
    }
}

/// Prices in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    /// Writing to the 5 minute prompt cache.
    pub cache_write_5m: f64,
    /// Writing to the 1 hour prompt cache.
    pub cache_write_1h: f64,
    /// Reading from the prompt cache.
    pub cache_read: f64,
}

impl Rates {
    fn is_valid(&self) -> bool {
        [
            self.input,
            self.output,
            self.cache_write_5m,
            self.cache_write_1h,
            self.cache_read,
        ]
        .iter()
        .all(|rate| rate.is_finite() && *rate > 0.0)
    }
}

/// Prices that replace the standard ones when a request's prompt (input,
/// cache writes and cache reads together) is longer than `over_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongContext {
    pub over_tokens: u64,
    pub rates: Rates,
}

/// The tokens of one API response, split the way they are priced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Uncached input.
    pub input: u64,
    pub output: u64,
    /// Cache writes with a 5 minute lifetime. When a transcript does not say
    /// which lifetime a write had, count it here.
    pub cache_write_5m: u64,
    /// Cache writes with a 1 hour lifetime.
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl Tokens {
    /// The prompt length: everything the model was given.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }
}

/// A price table that could not be used.
#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("the price table is not valid JSON of the expected shape: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the price table is inconsistent: {0}")]
    Invalid(String),
}

/// Strips what a transcript may add to a base model id: Claude Code's `[1m]`
/// marker and a snapshot date (`-20251001` or `@20251001`).
fn base_id(id: &str) -> &str {
    let id = id.strip_suffix("[1m]").unwrap_or(id);
    let bytes = id.as_bytes();
    let Some(cut) = bytes.len().checked_sub(9) else {
        return id;
    };
    let dated = matches!(bytes[cut], b'-' | b'@')
        && bytes[cut + 1..].starts_with(b"20")
        && bytes[cut + 1..].iter().all(u8::is_ascii_digit);
    // The nine bytes are ASCII, so `cut` is a character boundary.
    if dated {
        &id[..cut]
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MILLION: u64 = 1_000_000;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_embedded_table_is_valid() {
        let table = Pricing::from_json(EMBEDDED).unwrap();
        assert_eq!(&table, embedded());
        assert!(!table.models().is_empty());
        assert_eq!(table.as_of().len(), 10);
        assert!(table.source().starts_with("https://"));
    }

    /// The published multipliers: a cache write costs 1.25× (5 minutes) or
    /// 2× (1 hour) the input price, a cache read at most a tenth of it, and
    /// output five times it. A typo in the snapshot breaks one of these.
    #[test]
    fn the_embedded_rates_follow_the_published_multipliers() {
        for model in embedded().models() {
            let tiers = [Some(model.rates), model.long_context.map(|l| l.rates)];
            for rates in tiers.into_iter().flatten() {
                let id = &model.id;
                assert!(close(rates.cache_write_5m, rates.input * 1.25), "{id}");
                assert!(close(rates.cache_write_1h, rates.input * 2.0), "{id}");
                assert!(close(rates.output, rates.input * 5.0), "{id}");
                assert!(rates.cache_read <= rates.input * 0.1 + 1e-9, "{id}");
            }
        }
    }

    #[test]
    fn the_table_covers_the_current_models() {
        for id in [
            "claude-fable-5-2",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-haiku-4-5",
        ] {
            assert!(embedded().model(id).is_some(), "{id}");
        }
        let fable = embedded().model("claude-fable-5-2").unwrap();
        assert_eq!(fable.context_window, 1_000_000);
        assert_eq!(fable.rates.input, 8.0);
        assert_eq!(fable.rates.cache_read, 0.2);
        assert_eq!(
            embedded().model("claude-haiku-4-5").unwrap().context_window,
            200_000
        );
    }

    #[test]
    fn dated_and_suffixed_ids_resolve_to_the_base_model() {
        let table = embedded();
        for (alias, base) in [
            ("claude-haiku-4-5-20251001", "claude-haiku-4-5"),
            ("claude-opus-4-5-20251101", "claude-opus-4-5"),
            ("claude-opus-4-5@20251101", "claude-opus-4-5"),
            ("claude-sonnet-4-20250514", "claude-sonnet-4"),
            ("claude-opus-4-8[1m]", "claude-opus-4-8"),
            ("claude-haiku-4-5-20251001[1m]", "claude-haiku-4-5"),
        ] {
            assert_eq!(table.model(alias).map(|m| m.id.as_str()), Some(base));
        }
    }

    #[test]
    fn an_unknown_model_has_no_price() {
        let table = embedded();
        for id in [
            "",
            "<synthetic>",
            "claude-future-9",
            "gpt-4o",
            "claude-opus-5-5-preview",
            "-20251001",
            "日本語のモデル名です",
        ] {
            assert!(table.model(id).is_none(), "{id}");
        }
    }

    #[test]
    fn cost_prices_each_kind_of_token() {
        let opus = embedded().model("claude-opus-5-5").unwrap();
        let one_of_each = Tokens {
            input: MILLION,
            output: MILLION,
            cache_write_5m: MILLION,
            cache_write_1h: MILLION,
            cache_read: MILLION,
        };
        assert!(close(
            opus.cost(&one_of_each, false),
            4.0 + 20.0 + 5.0 + 8.0 + 0.2
        ));
        assert_eq!(opus.cost(&Tokens::default(), false), 0.0);

        let small = Tokens {
            input: 12,
            output: 80,
            cache_write_1h: 300,
            cache_read: 4000,
            ..Tokens::default()
        };
        let expected = (12.0 * 4.0 + 80.0 * 20.0 + 300.0 * 8.0 + 4000.0 * 0.2) / 1e6;
        assert!(close(opus.cost(&small, false), expected));
    }

    #[test]
    fn fast_mode_multiplies_every_price_where_it_exists() {
        let tokens = Tokens {
            input: MILLION,
            output: MILLION,
            cache_read: MILLION,
            ..Tokens::default()
        };
        let fable = embedded().model("claude-fable-5-2").unwrap();
        assert!(close(fable.cost(&tokens, true), 16.0 + 80.0 + 0.4));
        let sonnet = embedded().model("claude-sonnet-5-5").unwrap();
        assert_eq!(sonnet.cost(&tokens, true), sonnet.cost(&tokens, false));
    }

    #[test]
    fn a_long_prompt_moves_haiku_to_its_higher_prices() {
        let haiku = embedded().model("claude-haiku-5-5").unwrap();
        // The prompt is input, cache writes and cache reads together.
        let at_threshold = Tokens {
            input: 1_000,
            cache_read: 99_000,
            output: 1_000,
            ..Tokens::default()
        };
        let over = Tokens {
            input: 1_001,
            ..at_threshold
        };
        assert!(close(
            haiku.cost(&at_threshold, false),
            (1_000.0 * 0.10 + 99_000.0 * 0.01 + 1_000.0 * 0.50) / 1e6
        ));
        assert!(close(
            haiku.cost(&over, false),
            (1_001.0 * 0.50 + 99_000.0 * 0.05 + 1_000.0 * 2.50) / 1e6
        ));
    }

    #[test]
    fn a_malformed_table_is_rejected() {
        let model = |extra: &str| {
            format!(
                r#"{{"as_of":"2026-01-01","source":"s","unit":"u","models":[
                {{"id":"m","context_window":1,"rates":{{"input":1,"output":5,
                "cache_write_5m":1.25,"cache_write_1h":2,"cache_read":0.1}}{extra}}}]}}"#
            )
        };
        assert!(Pricing::from_json(&model("")).is_ok());
        assert!(matches!(
            Pricing::from_json("not json"),
            Err(PricingError::Json(_))
        ));
        assert!(matches!(
            Pricing::from_json(&model(r#","typo":1"#)),
            Err(PricingError::Json(_))
        ));
        assert!(matches!(
            Pricing::from_json(&model(r#","fast_multiplier":0.5"#)),
            Err(PricingError::Invalid(_))
        ));
        let zero_rate = model("").replace(r#""input":1,"#, r#""input":0,"#);
        assert!(matches!(
            Pricing::from_json(&zero_rate),
            Err(PricingError::Invalid(_))
        ));
        let dated_id = model("").replace(r#""id":"m""#, r#""id":"m-20260101""#);
        assert!(matches!(
            Pricing::from_json(&dated_id),
            Err(PricingError::Invalid(_))
        ));
        let twice = model("").replace(
            r#""models":["#,
            r#""models":[{"id":"m","context_window":1,"rates":{"input":1,"output":5,
            "cache_write_5m":1.25,"cache_write_1h":2,"cache_read":0.1}},"#,
        );
        assert!(matches!(
            Pricing::from_json(&twice),
            Err(PricingError::Invalid(_))
        ));
    }
}
