//! Thin binary entry point for the `vv` CLI. All logic lives in
//! `banned_words_service::cli` so the library test suite can exercise it.

use banned_words_service::cli;

fn main() -> std::process::ExitCode {
    cli::run()
}
