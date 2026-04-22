// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use arc_evm_specs_tests::result::RunStatus;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "arc-evm-specs-tests",
    version,
    about = "ARC EVM specs state-test runner"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run state tests from EEST fixtures
    Statetest {
        /// Path to a JSON fixture file or directory
        path: std::path::PathBuf,

        /// Run only the test with this name
        #[arg(long)]
        run: Option<String>,

        /// Rerun failing tests with EIP-3155 trace output on stderr
        #[arg(long)]
        trace: bool,

        /// Emit upstream-style per-test JSON outcome fields alongside name/pass/error
        #[arg(long)]
        json_outcome: bool,

        /// Exit with non-zero code if any test fails
        #[arg(long)]
        strict_exit: bool,
    },
}

fn main() {
    if emit_compat_version_if_requested() {
        return;
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Statetest {
            path,
            run: filter_name,
            trace,
            json_outcome,
            strict_exit,
        } => match arc_evm_specs_tests::cmd::statetest::run(
            path,
            filter_name,
            strict_exit,
            trace,
            json_outcome,
        ) {
            Ok(RunStatus::Success) => {}
            Ok(status) => std::process::exit(status as i32),
            Err(e) => {
                eprintln!("Fatal error: {e}");
                std::process::exit(2);
            }
        },
    }
}

fn emit_compat_version_if_requested() -> bool {
    emit_compat_version_if_requested_from_args(std::env::args().skip(1))
}

fn emit_compat_version_if_requested_from_args(mut args: impl Iterator<Item = String>) -> bool {
    let Some(first) = args.next() else {
        return false;
    };

    if args.next().is_some() {
        return false;
    }

    if matches!(first.as_str(), "-v" | "--version" | "version") {
        println!("evm version {}", env!("CARGO_PKG_VERSION"));
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::emit_compat_version_if_requested_from_args;

    #[test]
    fn compat_version_accepts_single_version_flags() {
        assert!(emit_compat_version_if_requested_from_args(
            ["-v".to_string()].into_iter()
        ));
        assert!(emit_compat_version_if_requested_from_args(
            ["--version".to_string()].into_iter()
        ));
        assert!(emit_compat_version_if_requested_from_args(
            ["version".to_string()].into_iter()
        ));
    }

    #[test]
    fn compat_version_rejects_other_shapes() {
        assert!(!emit_compat_version_if_requested_from_args(
            std::iter::empty()
        ));
        assert!(!emit_compat_version_if_requested_from_args(
            ["statetest".to_string()].into_iter()
        ));
        assert!(!emit_compat_version_if_requested_from_args(
            ["--version".to_string(), "extra".to_string()].into_iter()
        ));
    }
}
