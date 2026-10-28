//! simworld-cli: headless runner for the SimWorld core.
//!
//! Commands today (P3):
//! * `validate-profile <file>`: parse and validate a world profile JSON through the core
//!   (`simworld_core::profile`); prints `OK` (exit 0) or the error (exit 1). Warnings (unknown
//!   keys of a newer `profile_version`) go to stderr. This is the core's own parser and range
//!   check, not a generic JSON Schema validator.
//! * `default-profile`: print the compiled-in default world profile as JSON (the source of
//!   `addons/simworld/profiles/default.json`).
//!
//! * `seed airports | water | validate` (docs/spec-world-seed.md section 3.2, phase S0): the seed
//!   world's offline content tools (module `seed`).
//!
//! Fly scripts, cache bake, tile dumps and benches arrive in phase P6 of
//! docs/spec-library-packaging.md.

mod seed;

use std::process::ExitCode;

use simworld_core::{Log, Profile};

/// Log sink for the CLI: everything to stderr.
struct StderrLog;

impl Log for StderrLog {
    fn info(&self, msg: &str) {
        eprintln!("info: {msg}");
    }
    fn warn(&self, msg: &str) {
        eprintln!("warning: {msg}");
    }
    fn error(&self, msg: &str) {
        eprintln!("error: {msg}");
    }
}

fn usage() -> String {
    format!(
        "simworld-cli {} (simworld-core {}, world profile version {})\n\
         usage:\n  simworld-cli validate-profile <file.json>   check a world profile (exit 0 = OK)\n  \
         simworld-cli default-profile                print the default world profile\n\
         {}\n\
         planned (P6): fly, bake, tile, bench",
        env!("CARGO_PKG_VERSION"),
        simworld_core::VERSION,
        simworld_core::PROFILE_VERSION,
        seed::USAGE
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["validate-profile", path] => match simworld_core::profile::load_file(path, None, &StderrLog) {
            Ok(_) => {
                println!("OK");
                ExitCode::SUCCESS
            }
            Err(e) => {
                println!("{e}");
                ExitCode::FAILURE
            }
        },
        ["default-profile"] => {
            let json = serde_json::to_string_pretty(Profile::default().world()).unwrap_or_default();
            println!("{json}");
            ExitCode::SUCCESS
        }
        ["seed", ..] => match seed::run(&args[1..]) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        ["--version"] | ["-V"] => {
            println!("{}", usage().lines().next().unwrap_or_default());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}
