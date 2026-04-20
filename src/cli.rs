//! CLI surface for `vv`: argv parsing via clap-derive and subcommand
//! dispatch. Mirrors the server's matcher-facing API as a process-local
//! transport. See CLI_IMPLEMENTATION_PLAN.md (CM1+).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

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
        Command::Check(_) => ExitCode::SUCCESS,
        Command::Languages(_) => ExitCode::SUCCESS,
        Command::Version(_) => ExitCode::SUCCESS,
    }
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
        // Sanity: clap's help rendering includes our subcommand flags.
        // `--help` on a subcommand returns an Err carrying the rendered text.
        let err = Cli::try_parse_from(["vv", "check", "--help"]).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("--text"));
        assert!(rendered.contains("--json-input"));
        assert!(rendered.contains("--output"));
    }
}
