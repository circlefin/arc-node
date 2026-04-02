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
}

#[derive(Args, Clone, Debug, Default)]
pub struct MigrateCmd {
    /// Perform a dry-run without actually upgrading
    #[arg(long)]
    pub dry_run: bool,
}
