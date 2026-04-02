// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::FmtSubscriber;

use malachitebft_config::{LogFormat, LogLevel};

/// Initialize logging.
///
/// Returns a drop guard responsible for flushing any remaining logs when the program terminates.
/// The guard must be assigned to a binding that is not _, as _ will result in the guard being dropped immediately.
pub fn init(log_level: LogLevel, log_format: LogFormat) -> WorkerGuard {
    let filter = build_tracing_filter(log_level);

    let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stdout());

    // Construct a tracing subscriber with the supplied filter and enable reloading.
    let builder = FmtSubscriber::builder()
        .with_target(false)
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(enable_ansi())
        .with_thread_ids(false);

    // There must be a better way to use conditionals in the builder pattern.
    match log_format {
        LogFormat::Plaintext => {
            let subscriber = builder.finish();
            subscriber.init();
        }
        LogFormat::Json => {
            let subscriber = builder.json().finish();
            subscriber.init();
        }
    };

    guard
}

/// Check if both stdout and stderr are proper terminal (tty),
/// so that we know whether or not to enable colored output,
/// using ANSI escape codes. If either is not, eg. because
/// stdout is redirected to a file, we don't enable colored output.
pub fn enable_ansi() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::io::stderr().is_terminal()
}

/// Common prefixes of the crates targeted by the default log level.
const TARGET_CRATES: &[&str] = &["informalsystems_malachitebft", "arc", "circle"];

/// Build a tracing directive setting the log level for the
/// crates to the given `log_level`.
pub fn default_directive(log_level: LogLevel) -> String {
    use itertools::Itertools;

    TARGET_CRATES
        .iter()
        .map(|&c| format!("{c}={log_level}"))
        .join(",")
}

/// Builds a tracing filter based on the input `log_level`.
/// If the `RUST_LOG` environment variable is set, it takes precedence.
/// Returns error if the filter failed to build.
fn build_tracing_filter(log_level: LogLevel) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .parse(default_directive(log_level))
            .expect("valid tracing filter directive")
    })
}
