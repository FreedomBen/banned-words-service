//! Runtime configuration loader. Reads TOML (optional) then env (`BWS_*`),
//! with env winning on overlap. See IMPLEMENTATION_PLAN M3 item 1.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

const DEFAULT_TOML_PATH: &str = "/etc/bws/config.toml";
const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_MAX_INFLIGHT: usize = 1024;
const MIN_KEY_LEN_WARN: usize = 32;

/// Final, validated runtime config.
#[derive(Debug)]
pub struct Config {
    pub listen_addr: String,
    pub api_keys: Vec<Vec<u8>>,
    /// `None` ⇒ load every compiled language. Runtime allowlist validation
    /// against compiled codes lands in M4; M3 parses but doesn't gate on it.
    pub langs: Option<Vec<String>>,
    pub max_inflight: usize,
}

#[derive(Debug)]
pub enum ConfigError {
    TomlMissing(String),
    Parse(String),
    Invalid(String),
    NoApiKeys,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TomlMissing(p) => write!(f, "BWS_CONFIG_FILE={p} does not exist"),
            Self::Parse(m) => write!(f, "config parse error: {m}"),
            Self::Invalid(m) => write!(f, "invalid config: {m}"),
            Self::NoApiKeys => write!(
                f,
                "BWS_API_KEYS is required and must contain at least one non-empty entry"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Intermediate shape that figment deserializes into. Every field is
/// `Option<T>` so merge semantics are "later provider wins on the keys it
/// provides", which is what env-over-TOML requires.
#[derive(Deserialize, Default, Debug)]
struct RawConfig {
    listen_addr: Option<String>,
    api_keys: Option<StringOrList>,
    langs: Option<StringOrList>,
    max_inflight: Option<usize>,
}

/// TOML arrays stay arrays; env strings get comma-split downstream.
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum StringOrList {
    List(Vec<String>),
    Csv(String),
}

impl StringOrList {
    /// Materialize into a Vec, splitting on `,` only for the `Csv` variant.
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::List(v) => v,
            Self::Csv(s) => s.split(',').map(str::to_string).collect(),
        }
    }
}

/// Build the figment provider chain per the plan: TOML first, env second.
/// Returns the raw figment (un-extracted) so tests can substitute sources.
fn build_figment() -> Result<Figment, ConfigError> {
    let explicit_path = std::env::var("BWS_CONFIG_FILE").ok();
    let toml_path = explicit_path
        .clone()
        .unwrap_or_else(|| DEFAULT_TOML_PATH.to_string());
    let toml_exists = Path::new(&toml_path).exists();
    if explicit_path.is_some() && !toml_exists {
        return Err(ConfigError::TomlMissing(toml_path));
    }

    let mut f = Figment::new();
    if toml_exists {
        f = f.merge(Toml::file(&toml_path));
    }
    // `BWS_API_KEYS` → `api_keys`, etc. No underscore-splitting so multi-word
    // keys like `listen_addr` land on the right field.
    f = f.merge(Env::prefixed("BWS_").lowercase(true));
    Ok(f)
}

pub fn load() -> Result<Config, ConfigError> {
    let fig = build_figment()?;
    let raw: RawConfig = fig
        .extract()
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    assemble(raw)
}

/// The pure validation/shaping step, isolated from figment so it can be unit-
/// tested against hand-built `RawConfig` values.
fn assemble(raw: RawConfig) -> Result<Config, ConfigError> {
    let listen_addr = raw
        .listen_addr
        .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string());
    let api_keys = parse_api_keys(raw.api_keys)?;
    let langs = raw.langs.map(parse_langs).transpose()?;
    let max_inflight = raw.max_inflight.unwrap_or(DEFAULT_MAX_INFLIGHT);
    Ok(Config {
        listen_addr,
        api_keys,
        langs,
        max_inflight,
    })
}

fn parse_api_keys(src: Option<StringOrList>) -> Result<Vec<Vec<u8>>, ConfigError> {
    let Some(src) = src else {
        return Err(ConfigError::NoApiKeys);
    };
    let mut keys: Vec<Vec<u8>> = Vec::new();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    for raw in src.into_vec() {
        let k = raw.trim();
        if k.is_empty() {
            return Err(ConfigError::Invalid(
                "BWS_API_KEYS contains an empty entry".to_string(),
            ));
        }
        if k.len() < MIN_KEY_LEN_WARN {
            tracing::warn!(
                target: "config",
                len = k.len(),
                "BWS_API_KEYS entry shorter than {MIN_KEY_LEN_WARN} bytes"
            );
        }
        let kb = k.as_bytes().to_vec();
        if seen.insert(kb.clone()) {
            keys.push(kb);
        }
    }
    if keys.is_empty() {
        return Err(ConfigError::NoApiKeys);
    }
    Ok(keys)
}

fn parse_langs(src: StringOrList) -> Result<Vec<String>, ConfigError> {
    let mut langs: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in src.into_vec() {
        let l = raw.trim().to_ascii_lowercase();
        if l.is_empty() {
            return Err(ConfigError::Invalid(
                "BWS_LANGS contains an empty entry".to_string(),
            ));
        }
        if seen.insert(l.clone()) {
            langs.push(l);
        }
    }
    if langs.is_empty() {
        return Err(ConfigError::Invalid(
            "BWS_LANGS must contain at least one entry".to_string(),
        ));
    }
    Ok(langs)
}

#[cfg(test)]
// Figment's `Error` is ~208 bytes, which trips clippy::result_large_err on the
// `Jail::expect_with` closures' return type. The size is figment's choice and
// these closures run once per test, so boxing isn't worth it.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    fn raw_with_keys(keys: StringOrList) -> RawConfig {
        RawConfig {
            api_keys: Some(keys),
            ..Default::default()
        }
    }

    #[test]
    fn api_keys_whitespace_trimmed_from_csv() {
        let raw = raw_with_keys(StringOrList::Csv(
            "  key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa , key-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  "
                .into(),
        ));
        let cfg = assemble(raw).unwrap();
        assert_eq!(cfg.api_keys.len(), 2);
        assert_eq!(cfg.api_keys[0], b"key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(cfg.api_keys[1], b"key-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn api_keys_empty_entry_is_fatal() {
        let raw = raw_with_keys(StringOrList::Csv("key-ok,,key-two".into()));
        let err = assemble(raw).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)), "got {err:?}");
    }

    #[test]
    fn api_keys_deduplicated() {
        let raw = raw_with_keys(StringOrList::List(vec![
            "dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "other-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ]));
        let cfg = assemble(raw).unwrap();
        assert_eq!(cfg.api_keys.len(), 2);
    }

    #[test]
    fn api_keys_short_entry_accepted_but_warned() {
        // We can't easily assert the warn emission here without a test
        // subscriber; the path is exercised by the `.len() < 32` branch.
        let raw = raw_with_keys(StringOrList::Csv("short".into()));
        let cfg = assemble(raw).unwrap();
        assert_eq!(cfg.api_keys, vec![b"short".to_vec()]);
    }

    #[test]
    fn zero_keys_fatal_when_absent() {
        let err = assemble(RawConfig::default()).unwrap_err();
        assert!(matches!(err, ConfigError::NoApiKeys));
    }

    #[test]
    fn zero_keys_fatal_when_all_whitespace_only() {
        // A single whitespace-only entry trims to empty → Invalid (not NoApiKeys).
        let raw = raw_with_keys(StringOrList::Csv("   ".into()));
        let err = assemble(raw).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn langs_lowercased_and_deduped() {
        let raw = RawConfig {
            api_keys: Some(StringOrList::Csv(
                "key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            )),
            langs: Some(StringOrList::Csv("EN, ja ,en".into())),
            ..Default::default()
        };
        let cfg = assemble(raw).unwrap();
        assert_eq!(cfg.langs.unwrap(), vec!["en".to_string(), "ja".to_string()]);
    }

    #[test]
    fn defaults_applied_when_env_alone() {
        let raw = raw_with_keys(StringOrList::Csv(
            "key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ));
        let cfg = assemble(raw).unwrap();
        assert_eq!(cfg.listen_addr, DEFAULT_LISTEN_ADDR);
        assert_eq!(cfg.max_inflight, DEFAULT_MAX_INFLIGHT);
        assert!(cfg.langs.is_none());
    }

    #[test]
    fn env_overrides_toml() {
        Jail::expect_with(|jail: &mut Jail| {
            jail.create_file(
                "config.toml",
                r#"
                listen_addr = "127.0.0.1:7000"
                api_keys = ["from-toml-aaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
                max_inflight = 4
                "#,
            )?;
            jail.set_env("BWS_CONFIG_FILE", "config.toml");
            jail.set_env("BWS_LISTEN_ADDR", "0.0.0.0:9999");
            jail.set_env("BWS_MAX_INFLIGHT", "42");
            let fig = build_figment().expect("build figment");
            let raw: RawConfig = fig.extract()?;
            let cfg = assemble(raw).expect("assemble");
            assert_eq!(cfg.listen_addr, "0.0.0.0:9999");
            assert_eq!(cfg.max_inflight, 42);
            // api_keys was only in TOML, so TOML value survives.
            assert_eq!(cfg.api_keys.len(), 1);
            Ok(())
        });
    }

    #[test]
    fn absent_default_toml_yields_same_as_env_only() {
        Jail::expect_with(|jail: &mut Jail| {
            // Don't set BWS_CONFIG_FILE. Default /etc/bws/config.toml is
            // overwhelmingly unlikely to exist in a cargo test sandbox.
            jail.set_env("BWS_API_KEYS", "env-only-key-aaaaaaaaaaaaaaaaaaaaaaaaaaa");
            let fig = build_figment().expect("build figment");
            let raw: RawConfig = fig.extract()?;
            let cfg = assemble(raw).expect("assemble");
            assert_eq!(cfg.api_keys.len(), 1);
            assert_eq!(cfg.listen_addr, DEFAULT_LISTEN_ADDR);
            Ok(())
        });
    }

    #[test]
    fn explicit_missing_toml_is_fatal() {
        Jail::expect_with(|jail: &mut Jail| {
            jail.set_env("BWS_CONFIG_FILE", "definitely-not-there.toml");
            let err = build_figment().unwrap_err();
            assert!(matches!(err, ConfigError::TomlMissing(_)), "got {err:?}");
            Ok(())
        });
    }
}
