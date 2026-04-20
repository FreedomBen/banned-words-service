//! CLI surface for `vv`: argv parsing via clap-derive and subcommand
//! dispatch. Mirrors the server's matcher-facing API as a process-local
//! transport. See CLI_IMPLEMENTATION_PLAN.md (CM1+).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::matcher::{
    compiled_langs, Engine, Lang, Mode, NormalizeError, DEFAULT_MODE, LIST_VERSION, TERMS,
};
use crate::model::{CheckRequest, CheckResponse, LanguagesEntry, LanguagesResponse, MatchDto};

#[derive(Parser, Debug)]
#[command(
    name = "vv",
    version,
    about = "Vocab Veto — offline banned-words matcher",
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Scan text for banned words (mirrors the server's POST /v1/check).
    Check(CheckArgs),
    /// List compiled languages and their default modes.
    Languages(LanguagesArgs),
    /// Print crate and list versions.
    Version(VersionArgs),
}

/// Flag surface for `vv check`. Mutex rails are enforced by clap before
/// dispatch: `--text` / `--file` / `--stdin` are pairwise exclusive, and
/// `--json-input` excludes all three plus `--lang` / `--mode` (the JSON
/// body carries the equivalent fields).
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// Inline text to scan.
    #[arg(long, conflicts_with_all = ["file", "stdin", "json_input"])]
    pub text: Option<String>,

    /// Read text from file; `-` reads stdin.
    #[arg(long, conflicts_with_all = ["text", "stdin", "json_input"])]
    pub file: Option<PathBuf>,

    /// Read text from stdin.
    #[arg(long, conflicts_with_all = ["text", "file", "json_input"])]
    pub stdin: bool,

    /// Read a full CheckRequest JSON body (server shape). `-` reads stdin.
    #[arg(long, conflicts_with_all = ["text", "file", "stdin", "lang", "mode"])]
    pub json_input: Option<PathBuf>,

    /// Language code(s). Repeatable; also accepts comma-separated values.
    /// Omitted ⇒ scan every compiled language alphabetically.
    #[arg(long, value_delimiter = ',')]
    pub lang: Vec<String>,

    /// Override the per-language default mode (`strict` or `substring`).
    #[arg(long)]
    pub mode: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,

    /// Emit diagnostic lines to stderr (input length, mode resolution, etc.).
    #[arg(long, short = 'v')]
    pub verbose: bool,
}

#[derive(Args, Debug)]
pub struct LanguagesArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,
}

#[derive(Args, Debug)]
pub struct VersionArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub output: OutputFormat,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Plain,
}

/// Internal error categories mapped to exit codes in `run_check`. The
/// message is emitted on stderr verbatim; exit codes follow the table in
/// CLI_IMPLEMENTATION_PLAN.md §CM4.
#[derive(Debug)]
enum CliError {
    /// Invalid usage, malformed JSON, invalid mode, unknown language,
    /// empty text, empty langs — all collapsed to exit 2.
    Usage(String),
    /// File unreadable, stdin closed early, raw-text input not valid
    /// UTF-8. Exit 64.
    Io(String),
}

/// Entry point for `src/bin/vv.rs`. Returns the process exit code.
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap prints its own help / version / error text with the right
            // stream; follow its suggested exit code (0 for --help/--version,
            // 2 for parse errors).
            e.print().ok();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };

    match cli.command {
        Command::Check(args) => run_check(args),
        Command::Languages(args) => run_languages(args),
        Command::Version(args) => run_version(args),
    }
}

fn run_languages(_args: LanguagesArgs) -> ExitCode {
    // Mirrors routes/languages.rs: alphabetical code order, per-code
    // default_mode from DEFAULT_MODE (Substring is the conservative
    // fallback for any code without an explicit entry, matching the
    // server's identical unwrap_or). Since the CLI has no runtime
    // allowlist, the compiled set is the loaded set.
    let entries: Vec<LanguagesEntry> = compiled_langs()
        .into_iter()
        .map(|code| {
            let default_mode = DEFAULT_MODE
                .get(code)
                .copied()
                .unwrap_or(Mode::Substring);
            LanguagesEntry {
                code: code.to_string(),
                default_mode: default_mode.as_wire_str(),
            }
        })
        .collect();

    let resp = LanguagesResponse { languages: entries };
    if let Err(e) = write_json(&resp) {
        eprintln!("failed to write output: {e}");
        return ExitCode::from(64);
    }
    ExitCode::SUCCESS
}

fn run_version(_args: VersionArgs) -> ExitCode {
    // Shape: /readyz minus `ready` (always true for a local binary) plus
    // `crate_version`. See CLI_IMPLEMENTATION_PLAN.md §CM3.
    #[derive(serde::Serialize)]
    struct VersionResponse {
        crate_version: &'static str,
        list_version: &'static str,
        languages: usize,
    }
    let resp = VersionResponse {
        crate_version: env!("CARGO_PKG_VERSION"),
        list_version: LIST_VERSION,
        languages: TERMS.len(),
    };
    if let Err(e) = write_json(&resp) {
        eprintln!("failed to write output: {e}");
        return ExitCode::from(64);
    }
    ExitCode::SUCCESS
}

fn run_check(args: CheckArgs) -> ExitCode {
    let (text, scan_langs, mode) = match resolve_check_inputs(&args) {
        Ok(v) => v,
        Err(CliError::Usage(msg)) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
        Err(CliError::Io(msg)) => {
            eprintln!("{msg}");
            return ExitCode::from(64);
        }
    };

    let engine = build_engine();

    let result = match engine.scan(&text, &scan_langs, mode) {
        Ok(r) => r,
        Err(NormalizeError::TooLarge) => {
            eprintln!("input exceeds {} bytes after normalization", crate::matcher::MAX_NORMALIZED_BYTES);
            return ExitCode::from(3);
        }
    };

    let mut mode_used: BTreeMap<String, &'static str> = BTreeMap::new();
    for (lang, m) in result.mode_used {
        mode_used.insert(lang, m.as_wire_str());
    }

    let matches: Vec<MatchDto> = result
        .matches
        .into_iter()
        .map(|m| MatchDto {
            lang: m.lang,
            term: m.term,
            matched_text: m.matched_text,
            start: m.start,
            end: m.end,
        })
        .collect();

    let has_hits = !matches.is_empty() || result.truncated;

    let resp = CheckResponse {
        list_version: LIST_VERSION,
        mode_used,
        matches,
        truncated: result.truncated,
    };

    if let Err(e) = write_json(&resp) {
        eprintln!("failed to write output: {e}");
        return ExitCode::from(64);
    }

    if has_hits {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Build an `Engine` spanning every compiled language. The CLI has no
/// runtime allowlist (VV_LANGS is server-only; see CLI_IMPLEMENTATION_PLAN
/// mirror table), so the compiled set is the loaded set.
fn build_engine() -> Engine {
    let mut patterns: HashMap<Lang, &[&str]> = HashMap::with_capacity(TERMS.len());
    for (code, terms) in TERMS.entries() {
        patterns.insert((*code).to_string(), *terms);
    }
    Engine::new(&patterns)
}

/// Resolve the three inputs `engine.scan` needs: raw text, scan-lang
/// order, and an optional explicit mode. Mirrors routes/check.rs's
/// validation order so error rows match.
fn resolve_check_inputs(
    args: &CheckArgs,
) -> Result<(String, Vec<Lang>, Option<Mode>), CliError> {
    if let Some(path) = &args.json_input {
        let bytes = read_bytes_source(path.as_path())
            .map_err(|e| CliError::Io(format!("failed to read --json-input: {e}")))?;
        let req: CheckRequest = serde_json::from_slice(&bytes)
            .map_err(|e| CliError::Usage(format!("invalid JSON: {e}")))?;
        if req.text.is_empty() {
            return Err(CliError::Usage("empty text".into()));
        }
        let mode = resolve_mode(req.mode.as_deref())?;
        let scan_langs = match req.langs {
            Some(v) if v.is_empty() => return Err(CliError::Usage("empty langs".into())),
            Some(v) => validate_langs(&v)?,
            None => all_compiled_langs(),
        };
        return Ok((req.text, scan_langs, mode));
    }

    let text = read_text_argv(args)?;
    if text.is_empty() {
        return Err(CliError::Usage("empty text".into()));
    }

    let mode = resolve_mode(args.mode.as_deref())?;

    let scan_langs = if args.lang.is_empty() {
        all_compiled_langs()
    } else {
        // argv path: trim surrounding ASCII whitespace per entry, then
        // the downstream lowercase+membership check runs in validate_langs.
        // Order and repeats are preserved — `--lang en --lang en` scans en
        // twice to match the server's body-side `{"langs":["en","en"]}`.
        let processed: Vec<String> = args.lang.iter().map(|s| s.trim().to_string()).collect();
        validate_langs(&processed)?
    };

    Ok((text, scan_langs, mode))
}

fn all_compiled_langs() -> Vec<Lang> {
    compiled_langs().into_iter().map(String::from).collect()
}

fn resolve_mode(s: Option<&str>) -> Result<Option<Mode>, CliError> {
    match s {
        None => Ok(None),
        Some("strict") => Ok(Some(Mode::Strict)),
        Some("substring") => Ok(Some(Mode::Substring)),
        Some(other) => Err(CliError::Usage(format!(
            "invalid mode: {other} (expected 'strict' or 'substring')",
        ))),
    }
}

fn validate_langs(list: &[String]) -> Result<Vec<Lang>, CliError> {
    let mut out = Vec::with_capacity(list.len());
    for raw in list {
        let lower = raw.to_ascii_lowercase();
        if !TERMS.contains_key(lower.as_str()) {
            return Err(CliError::Usage(format!(
                "unknown language: {lower}. Compiled codes: [{}]",
                compiled_langs().join(", "),
            )));
        }
        out.push(lower);
    }
    Ok(out)
}

fn read_text_argv(args: &CheckArgs) -> Result<String, CliError> {
    if let Some(t) = &args.text {
        return Ok(t.clone());
    }
    if let Some(path) = &args.file {
        return read_string_source(path.as_path())
            .map_err(|e| CliError::Io(format!("failed to read --file: {e}")));
    }
    if args.stdin {
        return read_stdin_string().map_err(|e| CliError::Io(format!("failed to read stdin: {e}")));
    }
    // Default: read stdin when it's piped; error with a usage hint on TTY.
    if !io::stdin().is_terminal() {
        return read_stdin_string().map_err(|e| CliError::Io(format!("failed to read stdin: {e}")));
    }
    Err(CliError::Usage(
        "no input: pass --text, --file, --stdin, or --json-input (or pipe stdin)".into(),
    ))
}

fn read_string_source(path: &Path) -> io::Result<String> {
    if path == Path::new("-") {
        read_stdin_string()
    } else {
        std::fs::read_to_string(path)
    }
}

fn read_bytes_source(path: &Path) -> io::Result<Vec<u8>> {
    if path == Path::new("-") {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read(path)
    }
}

fn read_stdin_string() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn write_json<T: serde::Serialize>(value: &T) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, value)?;
    handle.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_text() {
        let cli = Cli::try_parse_from(["vv", "check", "--text", "hello", "--lang", "en"]).unwrap();
        match cli.command {
            Command::Check(args) => {
                assert_eq!(args.text.as_deref(), Some("hello"));
                assert_eq!(args.lang, vec!["en"]);
                assert_eq!(args.output, OutputFormat::Json);
                assert!(!args.verbose);
            }
            _ => panic!("expected check"),
        }
    }

    #[test]
    fn parses_languages() {
        let cli = Cli::try_parse_from(["vv", "languages"]).unwrap();
        assert!(matches!(cli.command, Command::Languages(_)));
    }

    #[test]
    fn parses_version() {
        let cli = Cli::try_parse_from(["vv", "version"]).unwrap();
        assert!(matches!(cli.command, Command::Version(_)));
    }

    #[test]
    fn unknown_subcommand_errors() {
        assert!(Cli::try_parse_from(["vv", "bogus"]).is_err());
    }

    #[test]
    fn check_text_and_file_conflict() {
        assert!(
            Cli::try_parse_from(["vv", "check", "--text", "a", "--file", "/tmp/x"]).is_err()
        );
    }

    #[test]
    fn check_text_and_stdin_conflict() {
        assert!(Cli::try_parse_from(["vv", "check", "--text", "a", "--stdin"]).is_err());
    }

    #[test]
    fn check_file_and_stdin_conflict() {
        assert!(
            Cli::try_parse_from(["vv", "check", "--file", "/tmp/x", "--stdin"]).is_err()
        );
    }

    #[test]
    fn json_input_conflicts_with_text() {
        assert!(Cli::try_parse_from([
            "vv", "check", "--json-input", "/tmp/x", "--text", "a",
        ])
        .is_err());
    }

    #[test]
    fn json_input_conflicts_with_file() {
        assert!(Cli::try_parse_from([
            "vv",
            "check",
            "--json-input",
            "/tmp/x",
            "--file",
            "/tmp/y",
        ])
        .is_err());
    }

    #[test]
    fn json_input_conflicts_with_stdin() {
        assert!(Cli::try_parse_from([
            "vv",
            "check",
            "--json-input",
            "/tmp/x",
            "--stdin",
        ])
        .is_err());
    }

    #[test]
    fn json_input_conflicts_with_lang() {
        assert!(Cli::try_parse_from([
            "vv",
            "check",
            "--json-input",
            "/tmp/x",
            "--lang",
            "en",
        ])
        .is_err());
    }

    #[test]
    fn json_input_conflicts_with_mode() {
        assert!(Cli::try_parse_from([
            "vv",
            "check",
            "--json-input",
            "/tmp/x",
            "--mode",
            "strict",
        ])
        .is_err());
    }

    #[test]
    fn lang_accepts_comma_separated_and_preserves_order() {
        let cli =
            Cli::try_parse_from(["vv", "check", "--text", "hi", "--lang", "zh,en,ja"]).unwrap();
        match cli.command {
            Command::Check(args) => assert_eq!(args.lang, vec!["zh", "en", "ja"]),
            _ => panic!("expected check"),
        }
    }

    #[test]
    fn lang_repeatable_preserves_order_without_dedup() {
        let cli = Cli::try_parse_from([
            "vv", "check", "--text", "hi", "--lang", "en", "--lang", "en",
        ])
        .unwrap();
        match cli.command {
            Command::Check(args) => assert_eq!(args.lang, vec!["en", "en"]),
            _ => panic!("expected check"),
        }
    }

    #[test]
    fn check_help_lists_output_flag() {
        let err = Cli::try_parse_from(["vv", "check", "--help"]).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("--text"));
        assert!(rendered.contains("--json-input"));
        assert!(rendered.contains("--output"));
    }

    #[test]
    fn resolve_mode_known_values() {
        assert_eq!(resolve_mode(None).unwrap(), None);
        assert_eq!(resolve_mode(Some("strict")).unwrap(), Some(Mode::Strict));
        assert_eq!(
            resolve_mode(Some("substring")).unwrap(),
            Some(Mode::Substring),
        );
    }

    #[test]
    fn resolve_mode_unknown_errors_usage() {
        let err = resolve_mode(Some("loose")).unwrap_err();
        match err {
            CliError::Usage(msg) => assert!(msg.contains("loose")),
            _ => panic!("expected Usage error"),
        }
    }

    #[test]
    fn validate_langs_lowercases_and_accepts_known() {
        let out = validate_langs(&["EN".to_string(), "Ja".to_string()]).unwrap();
        assert_eq!(out, vec!["en", "ja"]);
    }

    #[test]
    fn validate_langs_preserves_repeats() {
        let out = validate_langs(&["en".to_string(), "en".to_string()]).unwrap();
        assert_eq!(out, vec!["en", "en"]);
    }

    #[test]
    fn validate_langs_unknown_code_errors_with_listing() {
        let err = validate_langs(&["xx".to_string()]).unwrap_err();
        match err {
            CliError::Usage(msg) => {
                assert!(msg.contains("xx"));
                assert!(msg.contains("en"));
                assert!(msg.contains("ja"));
            }
            _ => panic!("expected Usage error"),
        }
    }

    #[test]
    fn validate_langs_empty_string_falls_through_to_unknown() {
        // Empty argv value `--lang ""` is distinct from the empty-list case
        // and is rejected as an unknown code, matching the plan.
        let err = validate_langs(&[String::new()]).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }
}
