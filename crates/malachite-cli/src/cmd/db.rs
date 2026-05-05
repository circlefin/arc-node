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

//! Database management commands

use clap::{Args, Subcommand};

#[derive(Subcommand, Clone, Debug)]
pub enum DbCommands {
    /// Migrate the database schema to latest version
    #[clap(alias = "upgrade")]
    Migrate(MigrateCmd),

    /// Compact the database to reclaim space. The node must be stopped before running this command.
    Compact,

    /// Roll back the database by removing heights. Dry-run by default; pass --execute to commit.
    #[clap(alias = "unwind")]
    Rollback(RollbackCmd),
}

#[derive(Args, Clone, Debug, Default)]
pub struct MigrateCmd {
    /// Perform a dry-run without actually upgrading
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Clone, Debug, Default)]
pub struct RollbackCmd {
    /// Number of heights to remove from the tip of the consensus DB.
    /// Mutually exclusive with --to-height.
    #[arg(long, value_name = "COUNT", conflicts_with = "to_height")]
    pub num_heights: Option<u64>,

    /// Absolute height to roll back to. All data above this height is removed.
    /// Mutually exclusive with --num-heights.
    #[arg(long, value_name = "HEIGHT", conflicts_with = "num_heights")]
    pub to_height: Option<u64>,

    /// Actually execute the rollback. Without this flag the command is a dry run.
    #[arg(long)]
    pub execute: bool,
}
