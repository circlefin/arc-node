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

use arc_engine_bench::cli::Cli;
use clap::Parser;
use color_eyre::eyre::Context;
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let filter = EnvFilter::builder()
        .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
        .from_env()
        .wrap_err("failed to initialize tracing filter")?;
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .wrap_err("failed to install tracing subscriber")?;

    tracing::info!(
        version = arc_version::SHORT_VERSION,
        commit = arc_version::GIT_COMMIT_HASH,
        "arc-engine-bench starting"
    );

    arc_engine_bench::run(Cli::parse().command).await
}
