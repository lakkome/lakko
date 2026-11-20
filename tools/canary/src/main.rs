// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `lakko-canary`: the synthetic canary of ADR-0096 section 5.
//!
//! ```text
//! lakko-canary init --api <https base> --sfu <ws(s) base> --state <path>
//!                   [--auth-url <base>] [--ds-url <base>] [--storage-url <base>]
//! lakko-canary run  --state <path> [--interval-secs 600] [--call-every 3] [--once]
//! ```
//!
//! `init` creates two dedicated accounts and their one-to-one and writes the
//! state file (service bases included, so `run` needs only `--state`).
//! `run` repeats the four steps (`sign_in`, `message`, `attachment`, `call`)
//! and exports `lakko_canary_*` metrics on `LAKKO_CANARY_METRICS_BIND` when it
//! is set. It is the only signal that a message was delivered and opened,
//! because no server can see that.

#![forbid(unsafe_code)]

mod account;
mod api;
mod call;
mod endpoints;
mod error;
mod init;
mod metrics;
mod runner;
mod schedule;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use tracing::error;
use tracing_subscriber::EnvFilter;

use crate::endpoints::Endpoints;
use crate::error::Error;

const USAGE: &str = "usage:
  lakko-canary init --api <https base> --sfu <ws(s) base> --state <path>
                    [--auth-url <base>] [--ds-url <base>] [--storage-url <base>]
  lakko-canary run  --state <path> [--interval-secs 600] [--call-every 3] [--once]";

/// Parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Init {
        endpoints: Endpoints,
        state: PathBuf,
    },
    Run {
        state: PathBuf,
        interval_secs: u64,
        call_every: u64,
        once: bool,
        ignore_account_age: bool,
    },
}

fn parse(args: &[String]) -> Result<Command, Error> {
    let usage = || Error::Usage(USAGE.to_owned());
    let (cmd, rest) = args.split_first().ok_or_else(usage)?;
    let mut flags: std::collections::BTreeMap<&str, Option<&str>> =
        std::collections::BTreeMap::new();
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        match flag {
            "--once" | "--ignore-account-age" => {
                flags.insert(flag, None);
                i += 1;
            },
            "--api" | "--sfu" | "--state" | "--auth-url" | "--ds-url" | "--storage-url"
            | "--interval-secs" | "--call-every" => {
                let value = rest.get(i + 1).ok_or_else(usage)?;
                flags.insert(flag, Some(value.as_str()));
                i += 2;
            },
            _ => return Err(usage()),
        }
    }
    let value = |name: &str| flags.get(name).copied().flatten();
    let state = PathBuf::from(value("--state").ok_or_else(usage)?);
    match cmd.as_str() {
        "init" => {
            for f in [
                "--interval-secs",
                "--call-every",
                "--once",
                "--ignore-account-age",
            ] {
                if flags.contains_key(f) {
                    return Err(usage());
                }
            }
            let endpoints = Endpoints::build(
                value("--api"),
                value("--auth-url"),
                value("--ds-url"),
                value("--storage-url"),
                value("--sfu").ok_or_else(usage)?,
            )?;
            Ok(Command::Init { endpoints, state })
        },
        "run" => {
            for f in ["--api", "--sfu", "--auth-url", "--ds-url", "--storage-url"] {
                if flags.contains_key(f) {
                    return Err(Error::Usage(
                        "run reads the service bases from the state file".to_owned(),
                    ));
                }
            }
            let number = |name: &str, default: u64| -> Result<u64, Error> {
                value(name).map_or(Ok(default), |v| v.parse().map_err(|_| usage()))
            };
            let interval_secs = number("--interval-secs", 600)?;
            if interval_secs == 0 {
                return Err(usage());
            }
            Ok(Command::Run {
                state,
                interval_secs,
                call_every: number("--call-every", 3)?,
                once: flags.contains_key("--once"),
                ignore_account_age: flags.contains_key("--ignore-account-age"),
            })
        },
        _ => Err(usage()),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            return ExitCode::from(2);
        },
    };
    match command {
        Command::Init { endpoints, state } => match init::run(endpoints, &state).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!(class = %e.class(), "canary init failed");
                ExitCode::FAILURE
            },
        },
        Command::Run {
            state,
            interval_secs,
            call_every,
            once,
            ignore_account_age,
        } => {
            if let Err(e) = lakko_metrics::start_from_env(
                "canary",
                env!("CARGO_PKG_VERSION"),
                "LAKKO_CANARY_METRICS_BIND",
            )
            .await
            {
                error!(error = %e, "canary metrics listener failed");
                return ExitCode::FAILURE;
            }
            let opts = runner::RunOptions {
                state_path: state,
                interval: Duration::from_secs(interval_secs),
                call_every,
                once,
                ignore_account_age,
            };
            match Box::pin(runner::run(opts)).await {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    error!(class = %e.class(), "canary run failed");
                    ExitCode::FAILURE
                },
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn init_parses_and_keeps_urls_for_the_state_file() {
        let c = parse(&args(
            "init --api https://lakko.me --sfu wss://test.lakko.me/sfu --state /x",
        ))
        .unwrap();
        let Command::Init { endpoints, state } = c else {
            panic!()
        };
        assert_eq!(state, PathBuf::from("/x"));
        assert_eq!(
            endpoints.sfu_connect_url(),
            "wss://test.lakko.me/sfu/v1/connect"
        );
    }

    #[test]
    fn run_takes_only_the_state_and_cadence() {
        let c = parse(&args(
            "run --state /s --interval-secs 300 --call-every 2 --once",
        ))
        .unwrap();
        assert_eq!(
            c,
            Command::Run {
                state: PathBuf::from("/s"),
                interval_secs: 300,
                call_every: 2,
                once: true,
                ignore_account_age: false,
            }
        );
        let c = parse(&args("run --state /s")).unwrap();
        assert!(matches!(
            c,
            Command::Run {
                interval_secs: 600,
                call_every: 3,
                once: false,
                ..
            }
        ));
        assert!(parse(&args("run --state /s --api https://x")).is_err());
        assert!(parse(&args("run --state /s --interval-secs 0")).is_err());
        assert!(parse(&args("run")).is_err());
        assert!(parse(&args("bogus --state /s")).is_err());
    }
}
