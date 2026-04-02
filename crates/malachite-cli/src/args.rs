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

//! Command-line interface arguments for a basic implementation.
//!
//! Read configuration from the configuration files found in the directory
//! provided with the `--home` global parameter.
//!
//! The command-line parameters are stored in the `Args` structure.
//! `clap` parses the command-line parameters into this structure.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use directories::BaseDirs;

use malachitebft_config::{LogFormat, LogLevel};

use crate::cmd::db::DbCommands;
use crate::cmd::download::DownloadCmd;
use crate::cmd::init::InitCmd;
use crate::cmd::key::KeyCmd;
use crate::cmd::start::StartCmd;
use crate::error::Error;

const APP_FOLDER: &str = ".arc/consensus";
const CONFIG_FILE: &str = "config.toml";
const GENESIS_FILE: &str = "genesis.json";
const PRIV_VALIDATOR_KEY_FILE: &str = "priv_validator_key.json";

#[derive(Parser, Clone, Debug, Default)]
#[command(
    name = "arc-node-consensus",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Arc consensus layer"
)]
pub struct Args {
    /// Home directory for the consensus layer (default: `~/.arc/consensus`)
    #[arg(long, global = true, value_name = "HOME_DIR")]
    pub home: Option<PathBuf>,

    /// Log level (default: `malachite=debug`)
    #[arg(long, global = true, value_name = "LOG_LEVEL")]
    pub log_level: Option<LogLevel>,

    /// Log format (default: `plaintext`)
    #[arg(long, global = true, value_name = "LOG_FORMAT")]
    pub log_format: Option<LogFormat>,

    #[command(subcommand)]
    pub command: Commands,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Clone, Debug)]
pub enum Commands {
    /// Start node
    Start(StartCmd),

    /// Initialize configuration
    Init(InitCmd),

    /// Display public key and address
    Key(KeyCmd),

    /// Database management commands
    #[command(subcommand)]
    Db(DbCommands),

    /// Download a consensus layer snapshot
    Download(DownloadCmd),
}

impl Default for Commands {
    fn default() -> Self {
        Commands::Start(StartCmd::default())
    }
}

impl Args {
    /// new returns a new instance of the arguments.
    pub fn new() -> Args {
        Args::parse()
    }

    /// get_home_dir returns the application home folder.
    /// Typically, `$HOME/.arc/consensus`, dependent on the operating system.
    pub fn get_home_dir(&self) -> Result<PathBuf, Error> {
        match self.home {
            Some(ref path) => Ok(path.clone()),
            None => Ok(BaseDirs::new()
                .ok_or(Error::DirPath)?
                .home_dir()
                .join(APP_FOLDER)),
        }
    }

    /// get_config_dir returns the configuration folder based on the home folder.
    pub fn get_config_dir(&self) -> Result<PathBuf, Error> {
        Ok(self.get_home_dir()?.join("config"))
    }

    /// get_config_file_path returns the configuration file path based on the command-line arguments
    /// and the configuration folder.
    pub fn get_config_file_path(&self) -> Result<PathBuf, Error> {
        Ok(self.get_config_dir()?.join(CONFIG_FILE))
    }

    /// get_genesis_file_path returns the genesis file path based on the command-line arguments and
    /// the configuration folder.
    pub fn get_genesis_file_path(&self) -> Result<PathBuf, Error> {
        Ok(self.get_config_dir()?.join(GENESIS_FILE))
    }

    /// get_db_path returns the database file path based on the home folder.
    pub fn get_db_path(&self) -> Result<PathBuf, Error> {
        Ok(self.get_home_dir()?.join("store.db"))
    }

    /// get_log_level_or_default returns the log level from the command-line or the default value.
    pub fn get_log_level_or_default(&self) -> LogLevel {
        self.log_level.unwrap_or_default()
    }

    /// get_priv_validator_key_file_path returns the private validator key file path based on the
    /// configuration folder.
    pub fn get_default_priv_validator_key_file_path(&self) -> Result<PathBuf, Error> {
        Ok(self.get_config_dir()?.join(PRIV_VALIDATOR_KEY_FILE))
    }
}
