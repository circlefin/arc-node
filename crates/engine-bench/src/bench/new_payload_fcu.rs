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

use super::{
    context::BenchContext,
    fixture::{PayloadFixture, PayloadFixtureMetadata},
    helpers::{duration_to_ms, fmt_hash, SUMMARY_FILE_NAME},
    output::{
        build_summary, throughput_mgas_per_s, throughput_tx_per_s, write_csv, CombinedLatencyRow,
        CsvWriter, COMBINED_LATENCY_FILE_NAME,
    },
};
use crate::cli::NewPayloadFcuArgs;
use arc_eth_engine::{json_structures::ExecutionBlock, rpc::ethereum_rpc::EthereumRPC};
use eyre::{bail, Context};
use std::time::Instant;
use tracing::info;

pub async fn run(args: NewPayloadFcuArgs) -> eyre::Result<()> {
    let context = BenchContext::new(&args.common, "new-payload-fcu")?;

    info!(
        payload_dir = %args.payload.display(),
        target_eth_rpc_url = args.target_eth_rpc_url,
        engine = %context.transport(),
        output_dir = %context.output_dir().display(),
        "running new-payload-fcu benchmark"
    );

    let target_eth_rpc = context.ethereum_rpc(&args.target_eth_rpc_url, "target eth rpc")?;
    let engine = context.engine().await?;
    let mut payload_fixture = PayloadFixture::open(args.payload.as_path())?;
    let metadata = payload_fixture.metadata().clone();
    verify_target_start_state(&target_eth_rpc, &metadata).await?;

    info!(
        from_block = metadata.from_block,
        to_block = metadata.to_block,
        payload_count = metadata.payload_count,
        "starting payload replay"
    );

    let benchmark_started = Instant::now();
    let row_capacity = metadata.payload_count.min(usize::MAX as u64) as usize;
    let mut rows = Vec::with_capacity(row_capacity);
    let mut csv_writer = CsvWriter::new(&context.output_dir().join(COMBINED_LATENCY_FILE_NAME))?;
    let mut cumulative_gas = 0_u64;
    let mut cumulative_txs = 0_u64;

    while let Some(payload) = payload_fixture.next_payload()? {
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let parent_hash = payload.payload_inner.payload_inner.parent_hash;
        let tx_count = payload.payload_inner.payload_inner.transactions.len() as u64;
        let gas_used = payload.payload_inner.payload_inner.gas_used;
        let block_number = payload.payload_inner.payload_inner.block_number;

        let start = Instant::now();
        let status = engine
            .new_payload(&payload, Vec::new(), parent_hash)
            .await
            .wrap_err_with(|| format!("engine_newPayloadV4 failed for block {block_number}"))?;
        let new_payload_latency = start.elapsed();

        if !status.is_valid() {
            bail!("engine_newPayloadV4 returned non-valid status for block {block_number}: {status:?}");
        }

        let fcu_result = engine
            .forkchoice_updated(block_hash, None)
            .await
            .wrap_err_with(|| {
                format!("engine_forkchoiceUpdatedV3 failed for block {block_number}")
            })?;
        let total_latency = start.elapsed();
        let fcu_latency = total_latency.saturating_sub(new_payload_latency);

        if !fcu_result.payload_status.is_valid() {
            bail!(
                "engine_forkchoiceUpdatedV3 returned non-valid status for block {block_number}: {:?}",
                fcu_result.payload_status
            );
        }

        cumulative_gas = cumulative_gas.saturating_add(gas_used);
        cumulative_txs = cumulative_txs.saturating_add(tx_count);
        let elapsed = benchmark_started.elapsed();

        let row = CombinedLatencyRow {
            block_number,
            block_hash: fmt_hash(block_hash),
            tx_count,
            gas_used,
            new_payload_ms: duration_to_ms(new_payload_latency),
            fcu_ms: duration_to_ms(fcu_latency),
            total_ms: duration_to_ms(total_latency),
            elapsed_ms: duration_to_ms(elapsed),
            mgas_per_s: throughput_mgas_per_s(gas_used, total_latency),
            tx_per_s: throughput_tx_per_s(tx_count, total_latency),
            cumulative_mgas_per_s: throughput_mgas_per_s(cumulative_gas, elapsed),
            cumulative_tx_per_s: throughput_tx_per_s(cumulative_txs, elapsed),
        };
        csv_writer.write_row(&row)?;
        rows.push(row);
    }

    csv_writer.finish()?;
    let wall_clock = benchmark_started.elapsed();

    let summary = build_summary("new-payload-fcu", &rows, wall_clock)?;
    write_csv(&context.output_dir().join(SUMMARY_FILE_NAME), &[summary])?;

    info!(
        samples = rows.len(),
        wall_clock_ms = duration_to_ms(wall_clock),
        output_dir = %context.output_dir().display(),
        "new-payload-fcu benchmark complete"
    );

    Ok(())
}

async fn verify_target_start_state(
    target_rpc: &EthereumRPC,
    metadata: &PayloadFixtureMetadata,
) -> eyre::Result<()> {
    let target_latest_block = target_rpc
        .get_block_by_number("latest")
        .await
        .wrap_err("failed to fetch latest block from target node")?
        .ok_or_else(|| eyre::eyre!("latest block not found on target node"))?;

    ensure_target_start_state(&target_latest_block, metadata)?;

    info!(
        target_block_number = target_latest_block.block_number,
        target_block_hash = %fmt_hash(target_latest_block.block_hash),
        "verified target node replay start state"
    );

    Ok(())
}

fn ensure_target_start_state(
    target_latest_block: &ExecutionBlock,
    metadata: &PayloadFixtureMetadata,
) -> eyre::Result<()> {
    if target_latest_block.block_number != metadata.expected_parent.block_number
        || target_latest_block.block_hash != metadata.expected_parent.block_hash
    {
        bail!(
            "target node is not at the expected replay start state: expected parent block {} ({}) before replaying block {}, but target latest is block {} ({})",
            metadata.expected_parent.block_number,
            fmt_hash(metadata.expected_parent.block_hash),
            metadata.from_block,
            target_latest_block.block_number,
            fmt_hash(target_latest_block.block_hash),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::fixture::ExpectedParentBlock;

    #[test]
    fn ensure_target_start_state_rejects_mismatch() {
        let expected_hash = format!("0x{}", "01".repeat(32)).parse().unwrap();
        let actual_hash = format!("0x{}", "02".repeat(32)).parse().unwrap();
        let target_latest_block = ExecutionBlock {
            block_hash: actual_hash,
            block_number: 10,
            parent_hash: format!("0x{}", "00".repeat(32)).parse().unwrap(),
            timestamp: 123,
        };
        let metadata = PayloadFixtureMetadata {
            from_block: 11,
            to_block: 12,
            payload_count: 2,
            expected_parent: ExpectedParentBlock {
                block_number: 10,
                block_hash: expected_hash,
            },
        };

        let err = ensure_target_start_state(&target_latest_block, &metadata).unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "target node is not at the expected replay start state: expected parent block 10 ({}) before replaying block 11, but target latest is block 10 ({})",
                fmt_hash(expected_hash),
                fmt_hash(actual_hash)
            )
        );
    }
}
