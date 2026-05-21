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

//! End-to-end check that `arc_validator_set_skipped_total` increments when a validator
//! with a malformed ed25519 public key is added to the on-chain active set.
//!
//! Drives the existing `ValidatorManagement.s.sol` admin script through `forge` to
//! register → configureController → activateValidator → updateVotingPowerUnsafe(20),
//! waits for the consensus layer to re-decode the validator set on the next decided
//! block, then asserts the counter incremented.
//!
//! Runs against the standard `localdev.toml` manifest:
//!
//! ```text
//! quake -f crates/quake/scenarios/localdev.toml start
//! quake test validator_set:malformed_key_skipped
//! ```
//!
//! Lives in the `validator_set` group, which is excluded from the default `quake test`
//! run (alongside `validation` and `health`) because it mutates persistent on-chain state.
//!
//! Local-only: requires `forge` on `PATH` and the Foundry default test mnemonic that
//! localdev's anvil seeds. Cleanup zeroes the malformed validator's voting power so the
//! testnet stays usable for subsequent runs.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use color_eyre::eyre::{bail, Context, Result};
use tracing::{info, warn};

use super::{quake_test, RpcClientFactory, TestParams, TestResult};
use crate::testnet::Testnet;

/// 32-byte non-zero blob that fails ed25519 curve decoding. Derived from `VALID_PK_A` in
/// `crates/eth-engine/src/abi_utils.rs` by flipping bit 3 of byte 13 (`0x40` → `0x48`).
/// Passes the contract's `requirePublicKeyBasicSanity` check (length=32, not all-zero) so
/// the registration broadcasts; the Rust decoder then rejects it as a malformed key,
/// triggering the `record_skipped_validator` callback that backs the counter.
const MALFORMED_PUBKEY: &str = "0xc992c8696818bda11d628f38584822a6332c144b7f929b4d972bd39a23244aec";

/// Anvil default test mnemonic, account #8. In localdev it holds the
/// VALIDATOR_REGISTERER, PERMISSIONED_VALIDATOR_MANAGER_OWNER, and CONTROLLER roles.
const ANVIL_KEY: &str = "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97";
const ANVIL_ADDR: &str = "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f";

const VALIDATOR_MGMT_SCRIPT: &str = "contracts/scripts/ValidatorManagement.s.sol";
const METRIC: &str = "arc_validator_set_skipped_total";

/// Voting power assigned to the malformed validator. Must be > 0 (the decoder filters
/// `votingPower == 0` before attempting key parse) but well below 1/3 of total to keep
/// the localdev quorum healthy. With 5 genesis validators each at the default power,
/// 20 is a comfortable margin.
const MALFORMED_VOTING_POWER: &str = "20";

/// Margin we wait for the consensus layer to fetch and re-decode the validator set after
/// the on-chain mutation. The `decided` handler runs the decoder on every new block, so
/// 5s comfortably covers >1 block period at the localdev block time (~500ms).
const DECODE_REFRESH_WAIT: Duration = Duration::from_secs(5);

#[quake_test(group = "validator_set", name = "malformed_key_skipped")]
fn malformed_key_skipped<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        if testnet.is_remote() {
            bail!("malformed_key_skipped: only supported on local testnets");
        }

        let el_urls = testnet.nodes_metadata.all_execution_urls();
        let (_, rpc_url) = el_urls
            .first()
            .ok_or_else(|| color_eyre::eyre::eyre!("no execution URLs in testnet"))?;
        let rpc_url = rpc_url.to_string();
        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
        let repo_root = testnet.repo_root_dir.clone();

        let baseline = total_skipped(&arc_checks::fetch_all_metrics(&metrics_urls).await);
        info!(baseline, "Baseline {METRIC} (sum across nodes)");

        // 1. registerValidator → capture registrationId from forge stdout.
        let stdout = forge_capture(
            &repo_root,
            &rpc_url,
            "registerValidator()",
            None,
            &[
                ("VALIDATOR_REGISTERER_KEY", ANVIL_KEY),
                ("VALIDATOR_PUBLIC_KEY_BYTES", MALFORMED_PUBKEY),
            ],
        )?;
        let reg_id = parse_registration_id(&stdout).ok_or_else(|| {
            color_eyre::eyre::eyre!("could not parse registrationId from forge output:\n{stdout}")
        })?;
        info!(registration_id = reg_id, "Registered malformed validator");

        // 2. configureController.
        forge(
            &repo_root,
            &rpc_url,
            "configureController()",
            None,
            &[
                ("PERMISSIONED_VALIDATOR_MANAGER_OWNER", ANVIL_KEY),
                ("CONTROLLER_ADDRESS", ANVIL_ADDR),
                ("REGISTRATION_ID", &reg_id.to_string()),
                ("CONTROLLER_VOTING_POWER_LIMIT", "100"),
            ],
        )?;

        // 3. activateValidator.
        forge(
            &repo_root,
            &rpc_url,
            "activateValidator()",
            None,
            &[("CONTROLLER_KEY", ANVIL_KEY)],
        )?;

        // 4. updateVotingPowerUnsafe(20). The unsafe variant skips the off-chain 1/3
        //    quorum simulation; we know 20 fits well under the threshold for localdev.
        forge(
            &repo_root,
            &rpc_url,
            "updateVotingPowerUnsafe(uint64)",
            Some(MALFORMED_VOTING_POWER),
            &[("CONTROLLER_KEY", ANVIL_KEY)],
        )?;

        info!(
            wait_secs = DECODE_REFRESH_WAIT.as_secs(),
            "Waiting for the next decided block(s) to re-decode the validator set"
        );
        tokio::time::sleep(DECODE_REFRESH_WAIT).await;

        let after = total_skipped(&arc_checks::fetch_all_metrics(&metrics_urls).await);
        info!(
            after,
            baseline, "Post-registration {METRIC} (sum across nodes)"
        );

        // Cleanup before potentially failing — leaves the testnet usable.
        if let Err(e) = forge(
            &repo_root,
            &rpc_url,
            "updateVotingPowerUnsafe(uint64)",
            Some("0"),
            &[("CONTROLLER_KEY", ANVIL_KEY)],
        ) {
            warn!("Cleanup updateVotingPowerUnsafe(0) failed: {e:#}");
        }

        if after <= baseline {
            bail!(
                "{METRIC} did not increment after registering a malformed validator: \
                 baseline_sum={baseline}, after_sum={after}"
            );
        }

        info!(
            increments = after - baseline,
            "Skipped-validator metric incremented"
        );
        Ok(())
    })
}

fn total_skipped(raw: &[(String, String)]) -> u64 {
    raw.iter()
        .map(|(_, body)| arc_checks::parse_counter(body, METRIC))
        .sum()
}

/// Foundry prints a `== Return ==` block for non-void script entrypoints; for our
/// `registerValidator()` it contains a line of the form `_registrationId: uint256 N`.
/// Match that delimiter and pull out `N`.
fn parse_registration_id(forge_output: &str) -> Option<u64> {
    forge_output
        .split("_registrationId: uint256")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|s| s.trim_matches(',').parse::<u64>().ok())
}

fn forge(
    repo_root: &Path,
    rpc_url: &str,
    sig: &str,
    sig_arg: Option<&str>,
    envs: &[(&str, &str)],
) -> Result<()> {
    let status = build_forge(repo_root, rpc_url, sig, sig_arg, envs)
        .status()
        .wrap_err_with(|| format!("spawn forge {sig}"))?;
    if !status.success() {
        bail!("forge script {sig} exited with {status}");
    }
    Ok(())
}

fn forge_capture(
    repo_root: &Path,
    rpc_url: &str,
    sig: &str,
    sig_arg: Option<&str>,
    envs: &[(&str, &str)],
) -> Result<String> {
    let output = build_forge(repo_root, rpc_url, sig, sig_arg, envs)
        .output()
        .wrap_err_with(|| format!("spawn forge {sig}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("forge script {sig} failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn build_forge(
    repo_root: &Path,
    rpc_url: &str,
    sig: &str,
    sig_arg: Option<&str>,
    envs: &[(&str, &str)],
) -> Command {
    let mut cmd = Command::new("forge");
    cmd.arg("script")
        .arg(VALIDATOR_MGMT_SCRIPT)
        .args(["--rpc-url", rpc_url])
        .arg("--broadcast")
        .args(["--sig", sig])
        .current_dir(repo_root);
    if let Some(arg) = sig_arg {
        cmd.arg(arg);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_registration_id_extracts_value() {
        let stdout = "\
== Return ==
_registrationId: uint256 6
";
        assert_eq!(parse_registration_id(stdout), Some(6));
    }

    #[test]
    fn parse_registration_id_handles_trailing_comma() {
        // Forge can emit comma-separated tuples for multi-return; even though our entry
        // has a single return, the parser should be robust if forge ever changes format.
        let stdout = "_registrationId: uint256 42, _other: bool true";
        assert_eq!(parse_registration_id(stdout), Some(42));
    }

    #[test]
    fn parse_registration_id_missing_returns_none() {
        assert_eq!(parse_registration_id("nothing here"), None);
    }
}
