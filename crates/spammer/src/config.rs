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

use std::path::PathBuf;

use crate::accounts::PartitionMode;
use color_eyre::eyre::{self, Result};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Default)]
pub enum GuzzlerFunction {
    #[default]
    HashLoop,
    StorageWrite,
    StorageRead,
    Guzzle,
    Guzzle2,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GuzzlerFnConfig {
    pub weight: u32,
    pub arg: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GuzzlerFnWeights {
    pub hash_loop: GuzzlerFnConfig,
    pub storage_write: GuzzlerFnConfig,
    pub storage_read: GuzzlerFnConfig,
    pub guzzle: GuzzlerFnConfig,
    pub guzzle2: GuzzlerFnConfig,
}

impl GuzzlerFnWeights {
    pub fn buckets(&self) -> [(GuzzlerFunction, u32); 5] {
        [
            (GuzzlerFunction::HashLoop, self.hash_loop.weight),
            (GuzzlerFunction::StorageWrite, self.storage_write.weight),
            (GuzzlerFunction::StorageRead, self.storage_read.weight),
            (GuzzlerFunction::Guzzle, self.guzzle.weight),
            (GuzzlerFunction::Guzzle2, self.guzzle2.weight),
        ]
    }

    pub fn total_weight(&self) -> u32 {
        self.hash_loop.weight
            + self.storage_write.weight
            + self.storage_read.weight
            + self.guzzle.weight
            + self.guzzle2.weight
    }

    pub fn arg_for(&self, function: GuzzlerFunction) -> u64 {
        match function {
            GuzzlerFunction::HashLoop => self.hash_loop.arg,
            GuzzlerFunction::StorageWrite => self.storage_write.arg,
            GuzzlerFunction::StorageRead => self.storage_read.arg,
            GuzzlerFunction::Guzzle => self.guzzle.arg,
            GuzzlerFunction::Guzzle2 => self.guzzle2.arg,
        }
    }

    pub fn validate_enabled_args(&self) -> std::result::Result<(), String> {
        let checks = [
            ("hash-loop", self.hash_loop),
            ("storage-write", self.storage_write),
            ("storage-read", self.storage_read),
            ("guzzle", self.guzzle),
            ("guzzle2", self.guzzle2),
        ];
        for (name, entry) in checks {
            if entry.weight > 0 && entry.arg == 0 {
                return Err(format!(
                    "Invalid guzzler fn weights: '{name}' has weight {} but arg is 0. \
Use '{name}=<weight>@<arg>' with arg > 0",
                    entry.weight
                ));
            }
        }
        Ok(())
    }
}

impl FromStr for GuzzlerFnWeights {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let mut out = GuzzlerFnWeights::default();

        for part in input.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((raw_key, raw_value)) = part.split_once('=') else {
                return Err(format!(
                    "Invalid guzzler fn entry '{part}'. Expected key=value or key=weight@arg"
                ));
            };
            let key = raw_key.trim().to_lowercase();
            let raw_value = raw_value.trim();
            let (raw_weight, raw_arg) = raw_value
                .split_once('@')
                .map_or((raw_value, None), |(weight, arg)| (weight, Some(arg)));
            let weight: u32 = raw_weight
                .trim()
                .parse()
                .map_err(|_| format!("Invalid weight '{raw_weight}' for '{raw_key}'"))?;
            let arg: u64 = match raw_arg {
                Some(a) => a
                    .trim()
                    .parse()
                    .map_err(|_| format!("Invalid arg '{a}' for '{raw_key}'"))?,
                None => 0,
            };
            let entry = GuzzlerFnConfig { weight, arg };

            match key.as_str() {
                "hash-loop" | "hash_loop" => out.hash_loop = entry,
                "storage-write" | "storage_write" => out.storage_write = entry,
                "storage-read" | "storage_read" => out.storage_read = entry,
                "guzzle" => out.guzzle = entry,
                "guzzle2" => out.guzzle2 = entry,
                _ => {
                    return Err(format!(
                        "Unknown guzzler fn key '{raw_key}'. Valid keys: hash-loop, storage-write, storage-read, guzzle, guzzle2"
                    ))
                }
            }
        }

        out.validate_enabled_args()?;
        Ok(out)
    }
}

/// ERC-20 function the spammer can call.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Erc20Function {
    #[default]
    Transfer,
    Approve,
    TransferFrom,
}

/// Relative weights for blending ERC-20 functions via `--erc20-fn-weights`.
///
/// Parsed from a comma-separated string such as `transfer=70,approve=20,transfer-from=10`.
/// Weights are ratios. When all weights are 0 (the default), the generator
/// defaults to 100% transfer for backward compatibility.
#[derive(Clone, Copy, Debug, Default)]
pub struct Erc20FnWeights {
    pub transfer: u32,
    pub approve: u32,
    pub transfer_from: u32,
}

impl Erc20FnWeights {
    pub fn buckets(&self) -> [(Erc20Function, u32); 3] {
        [
            (Erc20Function::Transfer, self.transfer),
            (Erc20Function::Approve, self.approve),
            (Erc20Function::TransferFrom, self.transfer_from),
        ]
    }

    pub fn total_weight(&self) -> u32 {
        self.transfer + self.approve + self.transfer_from
    }
}

impl FromStr for Erc20FnWeights {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let mut out = Erc20FnWeights::default();

        for part in input.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((raw_key, raw_value)) = part.split_once('=') else {
                return Err(format!(
                    "Invalid erc20 fn entry '{part}'. Expected key=weight (e.g., transfer=70)"
                ));
            };
            let key = raw_key.trim().to_lowercase();
            let raw_value = raw_value.trim();
            let weight: u32 = raw_value
                .parse()
                .map_err(|_| format!("Invalid weight '{raw_value}' for '{raw_key}'"))?;

            match key.as_str() {
                "transfer" => out.transfer = weight,
                "approve" => out.approve = weight,
                "transfer-from" | "transfer_from" => out.transfer_from = weight,
                _ => {
                    return Err(format!(
                        "Unknown erc20 fn key '{raw_key}'. Valid keys: transfer, approve, transfer-from"
                    ))
                }
            }
        }

        Ok(out)
    }
}

/// Transaction type the spammer can generate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TxType {
    /// Native USDC value transfer between prefunded accounts.
    #[default]
    Transfer,
    /// ERC-20 `transfer()` call on the TestToken contract.
    Erc20,
    /// Call to the GasGuzzler contract (function selected by `--guzzler-fn-weights`).
    Guzzler,
}

/// Relative weights for blending transaction types via `--mix`.
///
/// Parsed from a comma-separated string such as `transfer=70,erc20=20,guzzler=10`.
/// Weights are ratios, not percentages, so `transfer=2,erc20=1` produces ~67% transfers
/// and ~33% ERC-20 calls.
#[derive(Clone, Copy, Debug, Default)]
pub struct TxTypeMix {
    pub transfer: u32,
    pub erc20: u32,
    pub guzzler: u32,
}

impl TxTypeMix {
    pub fn buckets(&self) -> [(TxType, u32); 3] {
        [
            (TxType::Transfer, self.transfer),
            (TxType::Erc20, self.erc20),
            (TxType::Guzzler, self.guzzler),
        ]
    }

    pub fn total_weight(&self) -> u32 {
        self.transfer + self.erc20 + self.guzzler
    }
}

impl FromStr for TxTypeMix {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let mut out = TxTypeMix::default();

        for part in input.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((raw_key, raw_value)) = part.split_once('=') else {
                return Err(format!(
                    "Invalid mix entry '{part}'. Expected key=weight (e.g., transfer=70)"
                ));
            };
            let key = raw_key.trim().to_lowercase();
            let weight: u32 = raw_value
                .trim()
                .parse()
                .map_err(|_| format!("Invalid weight '{raw_value}' for '{raw_key}'"))?;

            match key.as_str() {
                "transfer" => out.transfer = weight,
                "erc20" => out.erc20 = weight,
                "guzzler" => out.guzzler = weight,
                _ => {
                    return Err(format!(
                        "Unknown tx type '{raw_key}'. Valid keys: transfer, erc20, guzzler"
                    ))
                }
            }
        }

        Ok(out)
    }
}

pub struct Config {
    /// Number of transaction generators to run in parallel
    pub num_generators: usize,
    /// How to partition the account space among generators
    pub partition_mode: PartitionMode,
    /// Maximum number of accounts to sign transactions
    pub max_num_accounts: usize,
    /// Whether to pre-initialize accounts with their signing keys and latest nonces
    pub preinit_accounts: bool,
    /// Whether to query the latest nonce from the node (for faster account initialization)
    pub query_latest_nonce: bool,
    /// Maximum number of transactions to send (all generators together)
    pub max_num_txs: u64,
    /// Maximum rate of transactions to send per second (all generators together)
    pub max_rate: u64,
    /// Maximum time in seconds to send transactions (0 for no limit)
    pub max_time: u64,
    /// Size of transaction input data in bytes
    pub tx_input_size: usize,
    /// Maximum number of transactions to send per account (0 for no limit)
    pub max_txs_per_account: u64,
    /// Whether to run in silent mode
    pub silent: bool,
    /// Whether to show the status of all transaction pools together with the output
    pub show_pool_status: bool,
    /// Whether to record submit-to-finalized transaction latency.
    pub tx_latency: bool,
    /// Whether to wait for the response from the node (for output error messages).
    /// Only applies in fire-and-forget mode; backpressure mode always waits.
    pub wait_response: bool,
    /// Whether to use fire-and-forget mode (buffered channel, optimistic nonces, no response wait)
    pub fire_and_forget: bool,
    /// Number of reconnection attempts when a connection fails
    pub reconnect_attempts: u32,
    /// Time to wait between reconnection attempts
    pub reconnect_period: std::time::Duration,
    /// Weighted transaction type mix (transfer, erc20, guzzler).
    ///
    /// Use `type=weight` (e.g., `transfer=70,erc20=20,guzzler=10`).
    /// When total weight is 0, defaults based on guzzler_fn_weights:
    /// if guzzler functions enabled, 100% guzzler; otherwise 100% transfer.
    pub tx_type_mix: TxTypeMix,
    /// Weighted function mix and per-function argument for GasGuzzler calls.
    ///
    /// A function is enabled only when weight > 0.
    /// Use `function=weight@arg` (e.g., hash-loop=70@2000).
    /// If total weight is 0, transfer transactions are used.
    pub guzzler_fn_weights: GuzzlerFnWeights,
    /// Weighted function mix for ERC-20 calls.
    ///
    /// When total weight is 0, defaults to 100% transfer (backward compatible).
    pub erc20_fn_weights: Erc20FnWeights,
    /// Directory for the latency CSV file.
    ///
    /// When `None`, the CSV is written to the current directory.
    /// The directory is created automatically if it does not
    /// exist.
    pub csv_dir: Option<PathBuf>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.num_generators == 0 {
            eyre::bail!("num_generators must be greater than 0");
        }
        if !self.max_num_accounts.is_multiple_of(self.num_generators) {
            eyre::bail!(
                "Expected max_num_accounts ({}) to be divisible by num_generators ({})",
                self.max_num_accounts,
                self.num_generators
            );
        }
        if !self.fire_and_forget && self.wait_response {
            eyre::bail!(
                "--wait-response requires --fire-and-forget (backpressure mode always waits)"
            );
        }
        if let Err(msg) = self.guzzler_fn_weights.validate_enabled_args() {
            eyre::bail!("{msg}");
        }
        if self.tx_type_mix.total_weight() == 0 {
            eyre::bail!("--mix total weight is 0; at least one tx type must have weight > 0");
        }
        if self.tx_type_mix.guzzler > 0 && self.guzzler_fn_weights.total_weight() == 0 {
            eyre::bail!(
                "--mix includes guzzler weight but --guzzler-fn-weights has no enabled functions"
            );
        }
        if self.tx_type_mix.erc20 > 0 && self.erc20_fn_weights.total_weight() == 0 {
            eyre::bail!(
                "--mix includes erc20 weight but --erc20-fn-weights has no enabled functions"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::time::Duration;

    fn default_config() -> Config {
        Config {
            num_generators: 1,
            partition_mode: PartitionMode::Linear,
            max_num_accounts: 100,
            preinit_accounts: false,
            query_latest_nonce: false,
            max_num_txs: 0,
            max_rate: 1000,
            max_time: 0,
            tx_input_size: 0,
            max_txs_per_account: 0,
            silent: false,
            show_pool_status: false,
            wait_response: false,
            fire_and_forget: false,
            reconnect_attempts: 3,
            reconnect_period: Duration::from_secs(3),
            tx_type_mix: TxTypeMix {
                transfer: 100,
                ..Default::default()
            },
            guzzler_fn_weights: GuzzlerFnWeights::default(),
            erc20_fn_weights: Erc20FnWeights {
                transfer: 100,
                ..Default::default()
            },
            tx_latency: false,
            csv_dir: None,
        }
    }

    #[test]
    fn tx_type_mix_parses_full_spec() {
        let mix = TxTypeMix::from_str("transfer=70,erc20=20,guzzler=10").expect("should parse");
        assert_eq!(mix.total_weight(), 100);
        assert_eq!(mix.transfer, 70);
        assert_eq!(mix.erc20, 20);
        assert_eq!(mix.guzzler, 10);
    }

    #[test]
    fn tx_type_mix_partial_spec_defaults_rest_to_zero() {
        let mix = TxTypeMix::from_str("erc20=50").expect("should parse");
        assert_eq!(mix.total_weight(), 50);
        assert_eq!(mix.transfer, 0);
        assert_eq!(mix.erc20, 50);
        assert_eq!(mix.guzzler, 0);
    }

    #[test]
    fn tx_type_mix_rejects_unknown_key() {
        let err = TxTypeMix::from_str("swap=50").expect_err("unknown key should fail");
        assert!(err.contains("Unknown tx type"));
    }

    #[test]
    fn tx_type_mix_rejects_invalid_weight() {
        let err = TxTypeMix::from_str("transfer=abc").expect_err("non-numeric should fail");
        assert!(err.contains("Invalid weight"));
    }

    #[test]
    fn config_rejects_guzzler_mix_without_fn_weights() {
        let config = Config {
            tx_type_mix: TxTypeMix::from_str("guzzler=100").unwrap(),
            guzzler_fn_weights: GuzzlerFnWeights::default(),
            ..default_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("guzzler weight but"));
    }

    #[test]
    fn config_rejects_all_zero_mix_weights() {
        let config = Config {
            tx_type_mix: TxTypeMix {
                transfer: 0,
                erc20: 0,
                guzzler: 0,
            },
            ..default_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("total weight is 0"));
    }

    #[test]
    fn config_accepts_erc20_mix_without_guzzler_fn_weights() {
        let config = Config {
            tx_type_mix: TxTypeMix::from_str("transfer=70,erc20=30").unwrap(),
            ..default_config()
        };
        config.validate().expect("should be valid");
    }

    #[test]
    fn config_rejects_erc20_mix_without_fn_weights() {
        let config = Config {
            tx_type_mix: TxTypeMix::from_str("erc20=100").unwrap(),
            erc20_fn_weights: Erc20FnWeights::default(),
            ..default_config()
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("erc20 weight but"));
    }

    #[test]
    fn guzzler_fn_weights_all_zero_is_valid_and_disables_guzzler() {
        let weights = GuzzlerFnWeights::from_str(
            "hash-loop=0,storage-write=0,storage-read=0,guzzle=0,guzzle2=0",
        )
        .expect("weights should parse");
        assert_eq!(weights.total_weight(), 0);
    }

    #[test]
    fn guzzler_fn_weights_with_args_parses() {
        let weights = GuzzlerFnWeights::from_str(
            "hash-loop=70@2000,storage-write=20@600,storage-read=10@500,guzzle=0,guzzle2=0",
        )
        .expect("weights should parse");
        assert_eq!(weights.total_weight(), 100);
        assert_eq!(weights.hash_loop.arg, 2000);
        assert_eq!(weights.storage_write.arg, 600);
        assert_eq!(weights.storage_read.arg, 500);
    }

    #[test]
    fn guzzler_fn_weights_partial_spec_defaults_rest_to_zero() {
        let weights =
            GuzzlerFnWeights::from_str("hash-loop=100@2000").expect("partial spec should parse");
        assert_eq!(weights.total_weight(), 100);
        assert_eq!(weights.hash_loop.weight, 100);
        assert_eq!(weights.hash_loop.arg, 2000);
        assert_eq!(weights.storage_write.weight, 0);
        assert_eq!(weights.guzzle.weight, 0);
    }

    #[test]
    fn guzzler_fn_weights_rejects_enabled_function_without_arg() {
        let err = GuzzlerFnWeights::from_str(
            "hash-loop=100,storage-write=0,storage-read=0,guzzle=0,guzzle2=0",
        )
        .expect_err("missing arg for enabled function should fail");
        assert!(err.contains("hash-loop"));
    }

    #[test]
    fn wait_response_requires_fire_and_forget() {
        let config = Config {
            fire_and_forget: false,
            wait_response: true,
            ..default_config()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("--wait-response requires --fire-and-forget"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn fire_and_forget_with_wait_response_is_valid() {
        let config = Config {
            fire_and_forget: true,
            wait_response: true,
            ..default_config()
        };
        config.validate().expect("should be valid");
    }

    #[test]
    fn backpressure_default_is_valid() {
        let config = default_config();
        config.validate().expect("should be valid");
    }

    #[test]
    fn erc20_fn_weights_parses_full_spec() {
        let weights = Erc20FnWeights::from_str("transfer=70,approve=20,transfer-from=10")
            .expect("should parse");
        assert_eq!(weights.total_weight(), 100);
        assert_eq!(weights.transfer, 70);
        assert_eq!(weights.approve, 20);
        assert_eq!(weights.transfer_from, 10);
    }

    #[test]
    fn erc20_fn_weights_partial_spec_defaults_rest_to_zero() {
        let weights = Erc20FnWeights::from_str("approve=50").expect("should parse");
        assert_eq!(weights.total_weight(), 50);
        assert_eq!(weights.transfer, 0);
        assert_eq!(weights.approve, 50);
        assert_eq!(weights.transfer_from, 0);
    }

    #[test]
    fn erc20_fn_weights_rejects_unknown_key() {
        let err = Erc20FnWeights::from_str("swap=50").expect_err("unknown key should fail");
        assert!(err.contains("Unknown erc20 fn key"));
    }

    #[test]
    fn erc20_fn_weights_rejects_invalid_weight() {
        let err = Erc20FnWeights::from_str("transfer=abc").expect_err("non-numeric should fail");
        assert!(err.contains("Invalid weight"));
    }

    #[test]
    fn erc20_fn_weights_default_all_zero() {
        let weights = Erc20FnWeights::default();
        assert_eq!(weights.total_weight(), 0);
    }

    #[test]
    fn erc20_fn_weights_accepts_underscore_key() {
        let weights =
            Erc20FnWeights::from_str("transfer_from=100").expect("underscore key should parse");
        assert_eq!(weights.transfer_from, 100);
    }
}
