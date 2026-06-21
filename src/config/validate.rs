//! Config validator. Runs after `serde` deserialization.
//!
//! Three tiers of strictness (per the v0.2.0 plan, item 5):
//!
//! | Tier        | Behavior                        | Source             |
//! | ----------- | ------------------------------- | ------------------ |
//! | Invalid     | Hard fail, refuse to start      | serde + post-parse |
//! | Deprecated  | Warn-once, continue with default| static allow-list  |
//! | Unknown     | Warn-once per field             | toml::Value walk   |
//!
//! Implementation: hand-rolled. No `validator` crate, no JSON schema —
//! the surface area is small enough that a focused pass is clearer
//! than a generic framework.
//!
//! The output is a [`ValidationOutcome`] with errors + warnings
//! collected separately. Callers (the server, the `check`
//! subcommand) decide what to do with each tier.

use super::types::RouterConfig;
use base64::Engine;
use std::collections::{HashMap, HashSet};

/// A single validation error. Path is bracket-annotated, e.g.
/// `[ratelimit.global].refill_per_minute`. Reason is human-readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

/// A validation warning. Same shape as `ConfigError`; the only
/// difference is how the caller reacts to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

/// Result of running the validator. Errors are fatal; warnings
/// are advisory. The `check` subcommand exits 1 on errors, 2 on
/// warnings only, 0 on clean.
#[derive(Debug, Default, Clone)]
pub struct ValidationOutcome {
    pub errors: Vec<ConfigError>,
    pub warnings: Vec<ConfigWarning>,
}

impl ValidationOutcome {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// Merge a serde deserialization error into the outcome as
    /// a single hard error. Serde's Display already includes the
    /// path; we surface the whole message rather than try to
    /// split multi-error strings.
    pub fn absorb_serde(&mut self, err: &toml::de::Error) {
        self.errors.push(ConfigError {
            path: "<root>".to_string(),
            reason: err.message().to_string(),
        });
    }
}

/// Top-level entry point. Reads raw TOML, runs serde + the
/// hand-rolled checks, returns the merged outcome.
pub fn validate(raw: &str) -> ValidationOutcome {
    let mut out = ValidationOutcome::default();

    // Pass 1: pre-parse as `toml::Value` so we can walk the tree
    // for unknown-field detection. The typed parse below is the
    // source of truth for "wrong type / missing required" — we
    // only use this pass for the unknown-field walker + as a
    // diagnostic when the typed parse fails.
    let generic = match raw.parse::<toml::Value>() {
        Ok(v) => Some(v),
        Err(e) => {
            out.errors.push(ConfigError {
                path: "<root>".to_string(),
                reason: format!("TOML syntax: {e}"),
            });
            None
        }
    };

    // Pass 2: typed parse. Catches wrong type, missing required,
    // and any other serde-level constraint.
    let config: RouterConfig = match toml::from_str(raw) {
        Ok(c) => c,
        Err(e) => {
            out.absorb_serde(&e);
            return out;
        }
    };

    // Pass 3: post-parse semantic checks (range, cross-field,
    // business rules). The validator's actual job.
    check_semantic(&config, &mut out);

    // Pass 4: walk the generic tree for unknown fields. Runs
    // after the typed parse so we only report unknowns the typed
    // schema didn't claim.
    if let Some(tree) = generic {
        collect_unknown_fields(&tree, &mut out);
    }

    // Pass 5: deprecation list. Static, hand-maintained. If a
    // field appears in the raw TOML AND in the deprecated set,
    // emit a warning with a migration hint.
    collect_deprecations(raw, &mut out);

    out
}

/// Hand-rolled semantic checks. All error paths are bracket-annotated
/// to match the visual hierarchy of the TOML file.
fn check_semantic(cfg: &RouterConfig, out: &mut ValidationOutcome) {
    // [server].bind: must parse as a SocketAddr. Empty is a
    // hard error so the user doesn't accidentally bind to a
    // default they didn't expect.
    if cfg.server.bind.trim().is_empty() {
        out.errors.push(ConfigError {
            path: "[server].bind".to_string(),
            reason: "must be set (e.g. \"0.0.0.0:8080\")".to_string(),
        });
    } else if cfg.server.bind.parse::<std::net::SocketAddr>().is_err() {
        out.errors.push(ConfigError {
            path: "[server].bind".to_string(),
            reason: format!("\"{}\" is not a valid host:port", cfg.server.bind),
        });
    }

    // [server].log_level: must be a valid tracing EnvFilter level.
    if !is_valid_log_level(&cfg.server.log_level) {
        out.errors.push(ConfigError {
            path: "[server].log_level".to_string(),
            reason: format!(
                "\"{}\" is not a valid level (trace|debug|info|warn|error)",
                cfg.server.log_level
            ),
        });
    }

    // [server].oauth_redirect_uri: when set, must be a parseable URL.
    if !cfg.server.oauth_redirect_uri.is_empty()
        && url::Url::parse(&cfg.server.oauth_redirect_uri).is_err()
    {
        out.errors.push(ConfigError {
            path: "[server].oauth_redirect_uri".to_string(),
            reason: format!("\"{}\" is not a valid URL", cfg.server.oauth_redirect_uri),
        });
    }

    // [[providers]]: each id must be non-empty, unique. base_url
    // and enc: key checked for shape.
    let mut seen_ids = HashSet::new();
    for (idx, p) in cfg.providers.iter().enumerate() {
        let path = format!("providers[{idx}]");
        if p.id.trim().is_empty() {
            out.errors.push(ConfigError {
                path: format!("{path}.id"),
                reason: "must not be empty".to_string(),
            });
        } else if !seen_ids.insert(p.id.clone()) {
            out.errors.push(ConfigError {
                path: format!("{path}.id"),
                reason: format!("duplicate provider id \"{}\"", p.id),
            });
        }
        if let Some(k) = &p.key {
            if let Some(rest) = k.strip_prefix("enc:") {
                if !looks_like_encrypted_blob(rest) {
                    out.errors.push(ConfigError {
                        path: format!("{path}.key"),
                        reason: "\"enc:\" prefix present but ciphertext is not valid base64"
                            .to_string(),
                    });
                }
            }
        }
        if let Some(b) = &p.base_url {
            if !b.is_empty() && url::Url::parse(b).is_err() {
                out.errors.push(ConfigError {
                    path: format!("{path}.base_url"),
                    reason: format!("\"{b}\" is not a valid URL"),
                });
            }
        }
    }

    // [tiers.*]: every primary must reference a known provider id.
    let provider_ids: HashSet<&str> = cfg.providers.iter().map(|p| p.id.as_str()).collect();
    for (tier_name, tier) in &cfg.tiers {
        let path = format!("tiers.{tier_name}");
        if tier.primary.is_empty() {
            out.errors.push(ConfigError {
                path: format!("{path}.primary"),
                reason: "must not be empty".to_string(),
            });
        } else if !provider_ids.contains(tier.primary.as_str()) {
            // Allow `provider/model` syntax — extract the prefix.
            let head = tier.primary.split('/').next().unwrap_or("");
            if !provider_ids.contains(head) {
                out.errors.push(ConfigError {
                    path: format!("{path}.primary"),
                    reason: format!(
                        "unknown provider \"{}\" (not in [[providers]])",
                        tier.primary
                    ),
                });
            }
        }
        for fb in &tier.fallbacks {
            let head = fb.split('/').next().unwrap_or("");
            if !provider_ids.contains(head) {
                out.errors.push(ConfigError {
                    path: format!("{path}.fallbacks"),
                    reason: format!("unknown fallback provider \"{fb}\""),
                });
            }
        }
        if let Some(down) = &tier.downgrade_to {
            if !cfg.tiers.contains_key(down) {
                out.errors.push(ConfigError {
                    path: format!("{path}.downgrade_to"),
                    reason: format!("unknown tier \"{down}\""),
                });
            }
        }
        if let Some(min_ctx) = tier.min_context_window {
            if min_ctx == 0 {
                out.errors.push(ConfigError {
                    path: format!("{path}.min_context_window"),
                    reason: "must be > 0 when set".to_string(),
                });
            }
        }
    }

    // [detection].rules: every tier reference must exist.
    for (idx, r) in cfg.detection.rules.iter().enumerate() {
        if !cfg.tiers.contains_key(&r.tier) {
            out.errors.push(ConfigError {
                path: format!("detection.rules[{idx}].tier"),
                reason: format!("unknown tier \"{}\"", r.tier),
            });
        }
    }

    // [retry]: ranges.
    if cfg.retry.request_budget_ms == 0 {
        out.errors.push(ConfigError {
            path: "[retry].request_budget_ms".to_string(),
            reason: "must be > 0 (infinite-loop guard)".to_string(),
        });
    }
    if cfg.retry.fixed_retry_wait_ms > cfg.retry.max_retry_after_ms {
        out.errors.push(ConfigError {
            path: "[retry]".to_string(),
            reason: format!(
                "fixed_retry_wait_ms ({}) > max_retry_after_ms ({})",
                cfg.retry.fixed_retry_wait_ms, cfg.retry.max_retry_after_ms
            ),
        });
    }

    // [budgets]: warn-fraction must be in (0, 1].
    if cfg.budgets.warn_fraction <= 0.0 || cfg.budgets.warn_fraction > 1.0 {
        out.errors.push(ConfigError {
            path: "[budgets].warn_fraction".to_string(),
            reason: "must be in (0, 1]".to_string(),
        });
    }
    if cfg.budgets.daily_cost_usd < 0.0 {
        out.errors.push(ConfigError {
            path: "[budgets].daily_cost_usd".to_string(),
            reason: "must be >= 0 (0 = unlimited)".to_string(),
        });
    }
    if cfg.budgets.per_request_cost_usd < 0.0 {
        out.errors.push(ConfigError {
            path: "[budgets].per_request_cost_usd".to_string(),
            reason: "must be >= 0 (0 = unlimited)".to_string(),
        });
    }

    // [pricing_sync].openrouter_url must be a URL.
    if !cfg.pricing_sync.openrouter_url.is_empty()
        && url::Url::parse(&cfg.pricing_sync.openrouter_url).is_err()
    {
        out.errors.push(ConfigError {
            path: "[pricing_sync].openrouter_url".to_string(),
            reason: format!("\"{}\" is not a valid URL", cfg.pricing_sync.openrouter_url),
        });
    }

    // [discovery].min_input_price_per_1m: must be >= 0.
    if cfg.discovery.min_input_price_per_1m < 0.0 {
        out.errors.push(ConfigError {
            path: "[discovery].min_input_price_per_1m".to_string(),
            reason: "must be >= 0".to_string(),
        });
    }

    // [specificity].rules: every primary must reference a known
    // provider, same rules as tier primaries.
    for (idx, r) in cfg.specificity.rules.iter().enumerate() {
        let head = r.primary.split('/').next().unwrap_or("");
        if !provider_ids.contains(head) {
            out.errors.push(ConfigError {
                path: format!("specificity.rules[{idx}].primary"),
                reason: format!("unknown provider in \"{}\"", r.primary),
            });
        }
        if let Some(t) = r.threshold {
            if t == 0 {
                out.errors.push(ConfigError {
                    path: format!("specificity.rules[{idx}].threshold"),
                    reason: "must be > 0 when set".to_string(),
                });
            }
        }
    }
}

fn is_valid_log_level(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "trace" | "debug" | "info" | "warn" | "warning" | "error"
    )
}

/// Quick check for "looks like base64". The real check happens
/// in the keystore at decrypt time; this just catches obvious
/// typos (e.g. `enc:hello world` would otherwise be accepted and
/// then fail at first use with a worse error).
fn looks_like_encrypted_blob(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .is_ok()
}

// --- Unknown-field detection -----------------------------------------

/// Recursive walker that compares the parsed `toml::Value` against
/// the declared schema. Any leaf not in the schema → warning.
fn collect_unknown_fields(tree: &toml::Value, out: &mut ValidationOutcome) {
    let Some(root) = tree.as_table() else {
        return;
    };
    for (key, value) in root {
        if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            out.warnings.push(ConfigWarning {
                path: key.clone(),
                reason: "unknown top-level field".to_string(),
            });
            continue;
        }
        // Resolve the section schema. Every known top-level key
        // either has a ValueSchema (regular tables + maps +
        // array-of-tables) or is a leaf scalar (e.g.
        // `log_retention_days`).
        if let Some(schema) = section(key) {
            walk_value(key, value, &schema, out);
        }
    }
}

fn walk_value(
    prefix: &str,
    value: &toml::Value,
    schema: &ValueSchema,
    out: &mut ValidationOutcome,
) {
    match schema {
        ValueSchema::Section(s) => walk_section(prefix, value, s, out),
        ValueSchema::ArrayOfSections(s) => {
            let Some(arr) = value.as_array() else {
                return;
            };
            for (idx, item) in arr.iter().enumerate() {
                walk_section(&format!("{prefix}[{idx}]"), item, s, out);
            }
        }
        ValueSchema::MapOfSections(s) => {
            // Map of tables: { tier_name = { ... } }.
            let Some(t) = value.as_table() else {
                return;
            };
            for (k, v) in t {
                walk_section(&format!("{prefix}.{k}"), v, s, out);
            }
        }
    }
}

fn walk_section(
    prefix: &str,
    value: &toml::Value,
    schema: &SectionSchema,
    out: &mut ValidationOutcome,
) {
    let Some(t) = value.as_table() else {
        return;
    };
    for (k, v) in t {
        if !schema.known.contains(&k.as_str()) {
            out.warnings.push(ConfigWarning {
                path: format!("{prefix}.{k}"),
                reason: "unknown field".to_string(),
            });
            continue;
        }
        if let Some(nested) = nested(prefix, k.as_str()) {
            walk_value(&format!("{prefix}.{k}"), v, &nested, out);
        }
    }
}

#[derive(Clone)]
struct SectionSchema {
    known: HashSet<&'static str>,
}

#[derive(Clone)]
enum ValueSchema {
    /// A sub-table.
    Section(SectionSchema),
    /// An array of sub-tables (i.e. `[[providers]]`).
    ArrayOfSections(SectionSchema),
    /// A map of sub-tables (i.e. `[tiers.NAME]`).
    MapOfSections(SectionSchema),
}

// --- Schema tables ----------------------------------------------------

/// Top-level keys we know about. Anything else is an unknown
/// field at root.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "server",
    "oauth_redirect_uri", // legacy top-level alias
    "auth",
    "database",
    "providers",
    "tiers",
    "tier_keys",
    "detection",
    "retry",
    "streaming",
    "specificity",
    "budgets",
    "pricing_sync",
    "discovery",
    "log_retention_days",
    // Future v0.2.0 sections; declared now so configs written
    // for v0.2.0 don't trigger "unknown field" warnings when
    // older v0.2.0-rc binaries load them.
    "ratelimit",
];

fn section(name: &str) -> Option<ValueSchema> {
    use ValueSchema::*;
    let m: HashMap<&'static str, ValueSchema> = HashMap::from([
        (
            "server",
            Section(SectionSchema {
                known: HashSet::from(["bind", "log_level", "oauth_redirect_uri"]),
            }),
        ),
        (
            "auth",
            Section(SectionSchema {
                known: HashSet::from(["enabled", "admin_key", "admin_password", "keys"]),
            }),
        ),
        (
            "database",
            Section(SectionSchema {
                known: HashSet::from(["path"]),
            }),
        ),
        (
            "providers",
            ArrayOfSections(SectionSchema {
                known: HashSet::from(["id", "type", "key", "base_url", "default_model", "path"]),
            }),
        ),
        (
            "tiers",
            MapOfSections(SectionSchema {
                known: HashSet::from([
                    "primary",
                    "fallbacks",
                    "allow_tier_downgrade",
                    "downgrade_to",
                    "min_context_window",
                    "timeouts",
                ]),
            }),
        ),
        (
            "tier_keys",
            // Map<string,string> — no further structure to check.
            // Walk at the leaf level; nothing inside to recurse.
            Section(SectionSchema {
                known: HashSet::new(),
            }),
        ),
        (
            "detection",
            Section(SectionSchema {
                known: HashSet::from([
                    "default_tier",
                    "session_window_minutes",
                    "session_lookback",
                    "rules",
                ]),
            }),
        ),
        (
            "retry",
            Section(SectionSchema {
                known: HashSet::from([
                    "max_same_provider_retries",
                    "fixed_retry_wait_ms",
                    "max_retry_after_ms",
                    "request_budget_ms",
                ]),
            }),
        ),
        (
            "streaming",
            Section(SectionSchema {
                known: HashSet::from(["buffer_threshold_tokens"]),
            }),
        ),
        (
            "specificity",
            Section(SectionSchema {
                known: HashSet::from(["enabled", "rules"]),
            }),
        ),
        (
            "budgets",
            Section(SectionSchema {
                known: HashSet::from(["daily_cost_usd", "per_request_cost_usd", "warn_fraction"]),
            }),
        ),
        (
            "pricing_sync",
            Section(SectionSchema {
                known: HashSet::from(["enabled", "interval_hours", "openrouter_url"]),
            }),
        ),
        (
            "discovery",
            Section(SectionSchema {
                known: HashSet::from(["enabled", "auto_assign_tiers", "min_input_price_per_1m"]),
            }),
        ),
        (
            "ratelimit",
            Section(SectionSchema {
                known: HashSet::from(["enabled", "global", "per_key"]),
            }),
        ),
    ]);
    m.get(name).cloned()
}

fn nested(parent: &str, key: &str) -> Option<ValueSchema> {
    use ValueSchema::*;
    let m: HashMap<(&'static str, &'static str), ValueSchema> = HashMap::from([
        (
            ("auth", "keys"),
            ArrayOfSections(SectionSchema {
                known: HashSet::from(["key", "name"]),
            }),
        ),
        (
            ("detection", "rules"),
            ArrayOfSections(SectionSchema {
                known: HashSet::from(["if", "tier"]),
            }),
        ),
        (
            ("specificity", "rules"),
            ArrayOfSections(SectionSchema {
                known: HashSet::from(["category", "primary", "threshold"]),
            }),
        ),
        (
            ("tiers", "fallbacks"),
            // Vec<String> — leaves, no further structure to walk.
            Section(SectionSchema {
                known: HashSet::new(),
            }),
        ),
        (
            ("tiers", "timeouts"),
            Section(SectionSchema {
                known: HashSet::from(["connect_ms", "read_ms", "per_attempt_ms"]),
            }),
        ),
        (
            ("ratelimit", "global"),
            Section(SectionSchema {
                known: HashSet::from(["refill_per_minute", "burst"]),
            }),
        ),
        (
            ("ratelimit", "per_key"),
            Section(SectionSchema {
                known: HashSet::from(["refill_per_minute", "burst"]),
            }),
        ),
    ]);
    m.get(&(parent, key)).cloned()
}

/// Static deprecation table. Each entry: `(path, hint)`.
const DEPRECATIONS: &[(&str, &str)] = &[
    // The pre-v0.2.0 top-level oauth_redirect_uri is still
    // accepted (main.rs prefers it over [server].oauth_redirect_uri),
    // but new configs should use [server].oauth_redirect_uri.
    (
        "oauth_redirect_uri",
        "top-level oauth_redirect_uri is deprecated; move under [server]",
    ),
];

fn collect_deprecations(raw: &str, out: &mut ValidationOutcome) {
    let Ok(tree) = raw.parse::<toml::Value>() else {
        return;
    };
    let Some(root) = tree.as_table() else {
        return;
    };
    for (path, hint) in DEPRECATIONS {
        let first = path.split('.').next().unwrap_or(path);
        if root.contains_key(first) {
            out.warnings.push(ConfigWarning {
                path: (*path).to_string(),
                reason: (*hint).to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ValidationOutcome {
        validate(s)
    }

    #[test]
    fn empty_string_is_clean() {
        let out = parse("");
        assert!(!out.has_errors(), "errors: {:?}", out.errors);
        assert!(!out.has_warnings(), "warnings: {:?}", out.warnings);
    }

    #[test]
    fn unknown_top_level_field_warns() {
        let out = parse("unknown_top_level = 1\n");
        assert!(!out.has_errors());
        assert!(out.warnings.iter().any(|w| w.path == "unknown_top_level"));
    }

    #[test]
    fn unknown_nested_field_warns() {
        let out = parse("[server]\nbind = \"0.0.0.0:8080\"\nbogus = 1\n");
        assert!(!out.has_errors());
        assert!(out.warnings.iter().any(|w| w.path == "server.bogus"));
    }

    #[test]
    fn bad_bind_address_errors() {
        let out = parse("[server]\nbind = \"not a socket\"\n");
        assert!(out.has_errors());
        assert!(out
            .errors
            .iter()
            .any(|e| e.path == "[server].bind" && e.reason.contains("not a valid host:port")));
    }

    #[test]
    fn empty_bind_errors() {
        let out = parse("[server]\nbind = \"\"\n");
        assert!(out.has_errors());
    }

    #[test]
    fn bad_log_level_errors() {
        let out = parse("[server]\nbind = \"0.0.0.0:8080\"\nlog_level = \"verbose\"\n");
        assert!(out.has_errors());
        assert!(out
            .errors
            .iter()
            .any(|e| e.path == "[server].log_level" && e.reason.contains("not a valid level")));
    }

    #[test]
    fn duplicate_provider_id_errors() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"

[[providers]]
id = "a"
type = "openai"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
        assert!(out
            .errors
            .iter()
            .any(|e| e.path == "providers[1].id" && e.reason.contains("duplicate")));
    }

    #[test]
    fn empty_provider_id_errors() {
        let toml = r#"
[[providers]]
id = ""
type = "openai"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn tier_referencing_unknown_provider_errors() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"

[tiers.simple]
primary = "b"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
        assert!(out
            .errors
            .iter()
            .any(|e| e.path == "tiers.simple.primary" && e.reason.contains("unknown provider")));
    }

    #[test]
    fn tier_with_provider_model_syntax_passes() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"

[tiers.simple]
primary = "a/gpt-4o"
"#;
        let out = parse(toml);
        assert!(!out.has_errors(), "errors: {:?}", out.errors);
    }

    #[test]
    fn malformed_enc_prefix_errors() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"
key = "enc:not-base64-!!!"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
        assert!(out
            .errors
            .iter()
            .any(|e| e.path == "providers[0].key" && e.reason.contains("enc:")));
    }

    #[test]
    fn valid_enc_prefix_passes() {
        let blob = base64::engine::general_purpose::STANDARD.encode([0u8; 28]);
        let toml = format!(
            r#"
[[providers]]
id = "a"
type = "openai"
key = "enc:{blob}"
"#
        );
        let out = parse(&toml);
        assert!(!out.has_errors(), "errors: {:?}", out.errors);
    }

    #[test]
    fn request_budget_zero_errors() {
        let toml = r#"
[retry]
request_budget_ms = 0
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn retry_wait_greater_than_max_errors() {
        let toml = r#"
[retry]
fixed_retry_wait_ms = 5000
max_retry_after_ms = 1000
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn warn_fraction_out_of_range_errors() {
        let toml = r#"
[budgets]
warn_fraction = 1.5
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn bad_oauth_url_errors() {
        let toml = r#"
[server]
bind = "0.0.0.0:8080"
oauth_redirect_uri = "not a url"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn detection_rule_unknown_tier_errors() {
        let toml = r#"
[detection]
[[detection.rules]]
tier = "nope"
[detection.rules.if]
has_tools = true
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn downgrade_to_unknown_tier_errors() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"

[tiers.simple]
primary = "a"
downgrade_to = "nope"
"#;
        let out = parse(toml);
        assert!(out.has_errors());
    }

    #[test]
    fn deprecation_warning_for_top_level_redirect_uri() {
        let toml = r#"
oauth_redirect_uri = "http://localhost:8080/cb"

[server]
bind = "0.0.0.0:8080"
"#;
        let out = parse(toml);
        assert!(out
            .warnings
            .iter()
            .any(|w| w.path == "oauth_redirect_uri" && w.reason.contains("deprecated")));
    }

    #[test]
    fn unknown_provider_field_warns() {
        let toml = r#"
[[providers]]
id = "a"
type = "openai"
frobnicate = 42
"#;
        let out = parse(toml);
        assert!(out.warnings.iter().any(|w| w.path.contains("frobnicate")));
    }
}
