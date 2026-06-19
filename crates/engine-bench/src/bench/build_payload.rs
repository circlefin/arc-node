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

use std::time::{Duration, Instant};

use alloy_primitives::B256;
use alloy_rpc_types_engine::{ExecutionPayloadV3, PayloadAttributes, PayloadId};
use arc_eth_engine::engine::EngineAPI;
use arc_eth_engine::{
    ENGINE_FORKCHOICE_UPDATED_TIMEOUT, ENGINE_GET_PAYLOAD_TIMEOUT, ENGINE_NEW_PAYLOAD_TIMEOUT,
};
use eyre::{bail, eyre, Context};
use tracing::{debug, info};

use crate::bench::context::BenchContext;
use crate::bench::fixture::PayloadFixture;
use crate::bench::helpers::{duration_to_ms, fmt_hash};
use crate::bench::new_payload_fcu::verify_target_start_state;
use crate::bench::output::{
    build_build_summary, write_csv, BuildSummaryRow, CombinedBuildLatencyRow, CsvWriter,
};
use crate::bench::tx_submit::TxSubmitter;
use crate::cli::{BuildPayloadArgs, GetPayloadVersion};

const COMBINED_BUILD_LATENCY_FILE_NAME: &str = "combined_build_latency.csv";
const BUILD_SUMMARY_FILE_NAME: &str = "build_summary.csv";

/// PayloadAttributes for a build on top of `parent_hash`, reusing the recorded block's
/// timestamp, prev_randao, and fee recipient. Arc has no beacon chain, so
/// parent_beacon_block_root carries the parent execution hash.
pub(crate) fn payload_attributes_from(
    recorded: &ExecutionPayloadV3,
    parent_hash: B256,
) -> PayloadAttributes {
    let inner = &recorded.payload_inner.payload_inner;
    PayloadAttributes {
        timestamp: inner.timestamp,
        prev_randao: inner.prev_randao,
        suggested_fee_recipient: inner.fee_recipient,
        withdrawals: Some(recorded.payload_inner.withdrawals.clone()),
        parent_beacon_block_root: Some(parent_hash),
        slot_number: None,
    }
}

pub(crate) fn use_v5_for(v: GetPayloadVersion) -> Option<bool> {
    match v {
        GetPayloadVersion::V5 => Some(true),
        GetPayloadVersion::V4 => Some(false),
        GetPayloadVersion::Auto => None,
    }
}

/// Call getPayload with the requested version. Auto tries V5 (Osaka) and falls back to V4
/// on an unsupported-fork error.
pub(crate) async fn get_payload_with_version(
    engine: &dyn EngineAPI,
    payload_id: PayloadId,
    version: GetPayloadVersion,
) -> eyre::Result<ExecutionPayloadV3> {
    match use_v5_for(version) {
        Some(use_v5) => {
            engine
                .get_payload(payload_id, use_v5, ENGINE_GET_PAYLOAD_TIMEOUT)
                .await
        }
        None => match engine
            .get_payload(payload_id, true, ENGINE_GET_PAYLOAD_TIMEOUT)
            .await
        {
            Ok(p) => Ok(p),
            Err(e) if is_unsupported_fork_err(&e) => {
                engine
                    .get_payload(payload_id, false, ENGINE_GET_PAYLOAD_TIMEOUT)
                    .await
            }
            Err(e) => Err(e),
        },
    }
}

/// Errors arrive as stringly eyre::Report, so match the JSON-RPC unsupported-fork code
/// (-38005) and the equivalent phrasings.
fn is_unsupported_fork_err(e: &eyre::Report) -> bool {
    let s = e.to_string().to_lowercase();
    s.contains("-38005")
        || s.contains("unsupported fork")
        || s.contains("method not found")
        || s.contains("getpayloadv5")
}

/// Inject the recorded transactions into the target mempool, returning the rejected count.
/// With `disallow_rejections`, the first rejection aborts the run.
async fn inject_transactions(
    submitter: &TxSubmitter,
    payload: &ExecutionPayloadV3,
    block_number: u64,
    disallow_rejections: bool,
) -> eyre::Result<u64> {
    let mut rejected = 0u64;
    for raw in &payload.payload_inner.payload_inner.transactions {
        if let Err(e) = submitter.send_raw_transaction(raw).await {
            if disallow_rejections {
                bail!("tx rejected for block {block_number}: {e}");
            }
            rejected = rejected.saturating_add(1);
            debug!(block_number, "tx rejected: {e}");
        }
    }
    Ok(rejected)
}

/// Built throughput in MGas/s over the build window plus getPayload latency; 0 when the
/// window is non-positive.
fn built_mgas_per_s(built_gas: u64, window_plus_get_s: f64) -> f64 {
    if window_plus_get_s > 0.0 {
        built_gas as f64 / window_plus_get_s / 1_000_000.0
    } else {
        0.0
    }
}

/// Ratio of built gas to recorded gas; 0 when the recorded block used no gas.
fn gas_fill_ratio(built_gas: u64, recorded_gas: u64) -> f64 {
    if recorded_gas > 0 {
        built_gas as f64 / recorded_gas as f64
    } else {
        0.0
    }
}

pub async fn run(args: BuildPayloadArgs) -> eyre::Result<()> {
    let context = BenchContext::new(&args.common, "build-payload")?;

    info!(
        payload_dir = %args.payload.display(),
        target_eth_rpc_url = args.target_eth_rpc_url,
        engine = %context.transport(),
        build_window_ms = args.build_window_ms,
        output_dir = %context.output_dir().display(),
        "running build-payload benchmark"
    );

    let eth_rpc = context.ethereum_rpc(&args.target_eth_rpc_url, "target eth rpc")?;
    let engine = context.engine().await?;
    let mut fixture = PayloadFixture::open(args.payload.as_path())?;
    let metadata = fixture.metadata().clone();
    verify_target_start_state(&eth_rpc, &metadata).await?;

    info!(
        from_block = metadata.from_block,
        to_block = metadata.to_block,
        payload_count = metadata.payload_count,
        "starting build-payload loop"
    );

    let submitter = TxSubmitter::new(
        args.target_eth_rpc_url.clone(),
        args.common.eth_rpc_timeout_ms,
    )?;
    let mut parent_hash = metadata.expected_parent.block_hash;

    let started = Instant::now();
    let row_capacity = metadata.payload_count.min(usize::MAX as u64) as usize;
    let mut rows: Vec<CombinedBuildLatencyRow> = Vec::with_capacity(row_capacity);
    let mut csv = CsvWriter::new(&context.output_dir().join(COMBINED_BUILD_LATENCY_FILE_NAME))?;

    while let Some(payload) = fixture.next_payload()? {
        let block_number = payload.payload_inner.payload_inner.block_number;
        let recorded_gas = payload.payload_inner.payload_inner.gas_used;
        let recorded_tx_count = payload.payload_inner.payload_inner.transactions.len() as u64;
        let recorded_block_hash = payload.payload_inner.payload_inner.block_hash;

        let rejected = inject_transactions(
            &submitter,
            &payload,
            block_number,
            args.disallow_tx_rejections,
        )
        .await?;

        // Start a build on the parent using the recorded block's attributes.
        let attrs = payload_attributes_from(&payload, parent_hash);
        let t0 = Instant::now();
        let fcu = engine
            .forkchoice_updated(parent_hash, Some(attrs), ENGINE_FORKCHOICE_UPDATED_TIMEOUT)
            .await
            .wrap_err_with(|| format!("FCU-with-attrs failed at block {block_number}"))?;
        let fcu_attrs_ms = duration_to_ms(t0.elapsed());
        if !fcu.payload_status.is_valid() {
            bail!(
                "FCU-attrs non-valid at block {block_number}: {:?}",
                fcu.payload_status
            );
        }
        let payload_id = fcu
            .payload_id
            .ok_or_else(|| eyre!("no payload_id from FCU-with-attrs at block {block_number}"))?;

        tokio::time::sleep(Duration::from_millis(args.build_window_ms)).await;

        let t1 = Instant::now();
        let built = get_payload_with_version(engine.as_ref(), payload_id, args.get_payload_version)
            .await
            .wrap_err_with(|| format!("getPayload failed at block {block_number}"))?;
        let get_payload_ms = duration_to_ms(t1.elapsed());
        let built_gas = built.payload_inner.payload_inner.gas_used;
        let built_tx_count = built.payload_inner.payload_inner.transactions.len() as u64;
        let built_hash = built.payload_inner.payload_inner.block_hash;

        // The built block is discarded; committing the recorded block keeps state on real
        // history and identical across flag variants.
        let status = engine
            .new_payload(
                &payload,
                Vec::new(),
                parent_hash,
                ENGINE_NEW_PAYLOAD_TIMEOUT,
            )
            .await
            .wrap_err_with(|| format!("newPayload(recorded) failed at block {block_number}"))?;
        if !status.is_valid() {
            bail!("recorded newPayload non-valid at block {block_number}: {status:?}");
        }
        let fcu2 = engine
            .forkchoice_updated(recorded_block_hash, None, ENGINE_FORKCHOICE_UPDATED_TIMEOUT)
            .await
            .wrap_err_with(|| format!("advance FCU failed at block {block_number}"))?;
        if !fcu2.payload_status.is_valid() {
            bail!(
                "advance FCU non-valid at block {block_number}: {:?}",
                fcu2.payload_status
            );
        }

        let elapsed_ms = duration_to_ms(started.elapsed());
        let window_plus_get_s = (args.build_window_ms as f64 + get_payload_ms) / 1000.0;
        let row = CombinedBuildLatencyRow {
            block_number,
            parent_hash: fmt_hash(parent_hash),
            recorded_gas,
            recorded_tx_count,
            built_block_hash: fmt_hash(built_hash),
            built_gas,
            built_tx_count,
            txs_submitted: recorded_tx_count,
            txs_rejected: rejected,
            fcu_attrs_ms,
            get_payload_ms,
            build_window_ms: args.build_window_ms as f64,
            elapsed_ms,
            built_mgas_per_s: built_mgas_per_s(built_gas, window_plus_get_s),
            gas_fill_ratio: gas_fill_ratio(built_gas, recorded_gas),
        };
        csv.write_row(&row)?;
        rows.push(row);

        parent_hash = recorded_block_hash;
        info!(
            block_number,
            built_gas, recorded_gas, get_payload_ms, "built + advanced"
        );
    }

    csv.finish()?;
    let wall_clock = started.elapsed();
    let summary: BuildSummaryRow =
        build_build_summary(&rows, wall_clock, args.build_window_ms as f64);
    write_csv(
        &context.output_dir().join(BUILD_SUMMARY_FILE_NAME),
        &[summary],
    )?;

    info!(
        samples = rows.len(),
        wall_clock_ms = duration_to_ms(wall_clock),
        output_dir = %context.output_dir().display(),
        "build-payload benchmark complete"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, Address};
    use serde_json::json;

    fn sample_payload(ts: u64, fee: Address, randao: B256) -> ExecutionPayloadV3 {
        let mut p: ExecutionPayloadV3 = serde_json::from_value(json!({
            "parentHash": format!("0x{}", "00".repeat(32)),
            "feeRecipient": format!("0x{}", "00".repeat(20)),
            "stateRoot": format!("0x{}", "01".repeat(32)),
            "receiptsRoot": format!("0x{}", "02".repeat(32)),
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "prevRandao": format!("0x{}", "00".repeat(32)),
            "blockNumber": "0x1",
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x3e8",
            "timestamp": "0x1",
            "extraData": "0x",
            "baseFeePerGas": "0x1",
            "blockHash": format!("0x{}", "03".repeat(32)),
            "transactions": [],
            "withdrawals": [],
            "blobGasUsed": "0x0",
            "excessBlobGas": "0x0"
        }))
        .expect("valid payload json");
        p.payload_inner.payload_inner.timestamp = ts;
        p.payload_inner.payload_inner.fee_recipient = fee;
        p.payload_inner.payload_inner.prev_randao = randao;
        p
    }

    #[test]
    fn version_maps_to_use_v5_flag() {
        assert_eq!(use_v5_for(GetPayloadVersion::V5), Some(true));
        assert_eq!(use_v5_for(GetPayloadVersion::V4), Some(false));
        assert_eq!(use_v5_for(GetPayloadVersion::Auto), None);
    }

    #[test]
    fn attributes_copy_recorded_fields() {
        let fee = address!("0x65E0a200006D4FF91bD59F9694220dafc49dbBC1");
        let randao = b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
        let parent = b256!("0x2222222222222222222222222222222222222222222222222222222222222222");
        let p = sample_payload(1234, fee, randao);
        let attrs = payload_attributes_from(&p, parent);
        assert_eq!(attrs.timestamp, 1234);
        assert_eq!(attrs.suggested_fee_recipient, fee);
        assert_eq!(attrs.prev_randao, randao);
        assert_eq!(attrs.parent_beacon_block_root, Some(parent));
        assert!(attrs.withdrawals.is_some());
    }
}
