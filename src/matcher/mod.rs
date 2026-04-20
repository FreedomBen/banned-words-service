//! Matcher library: compile-time term tables + runtime scan engine.
//!
//! `LIST_VERSION` and `TERMS` are pulled in from the build-script-generated
//! file in `$OUT_DIR` — see `build.rs`. The automaton map is built at startup
//! from `TERMS` (M3+); this module exposes the building blocks.

pub mod boundary;
pub mod normalize;
pub mod scan;

pub use boundary::is_word_boundary;
pub use normalize::{normalize, NormalizeError, Normalized, MAX_NORMALIZED_BYTES};
pub use scan::{Engine, Match, Mode, ScanResult, MAX_MATCHES};

/// Language code, lowercase ASCII (ISO 639-1 where available). See
/// DESIGN §"POST /v1/check" and IMPLEMENTATION_PLAN M3 item 4.
pub type Lang = String;

include!(concat!(env!("OUT_DIR"), "/generated_terms.rs"));

/// Per-language mode default, keyed by the 27 LDNOOBW ISO codes at the pinned
/// SHA. `Substring` for scripts without reliable inter-word spaces (ja, ko, th,
/// zh); `Strict` for everything else. See IMPLEMENTATION_PLAN M3 item 4.
pub static DEFAULT_MODE: ::phf::Map<&'static str, Mode> = ::phf::phf_map! {
    "ar"  => Mode::Strict,
    "cs"  => Mode::Strict,
    "da"  => Mode::Strict,
    "de"  => Mode::Strict,
    "en"  => Mode::Strict,
    "eo"  => Mode::Strict,
    "es"  => Mode::Strict,
    "fa"  => Mode::Strict,
    "fi"  => Mode::Strict,
    "fil" => Mode::Strict,
    "fr"  => Mode::Strict,
    "hi"  => Mode::Strict,
    "hu"  => Mode::Strict,
    "it"  => Mode::Strict,
    "ja"  => Mode::Substring,
    "kab" => Mode::Strict,
    "ko"  => Mode::Substring,
    "nl"  => Mode::Strict,
    "no"  => Mode::Strict,
    "pl"  => Mode::Strict,
    "pt"  => Mode::Strict,
    "ru"  => Mode::Strict,
    "sv"  => Mode::Strict,
    "th"  => Mode::Substring,
    "tlh" => Mode::Strict,
    "tr"  => Mode::Strict,
    "zh"  => Mode::Substring,
};
