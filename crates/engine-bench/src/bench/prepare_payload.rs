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
    context::ethereum_rpc_client,
    fixture::{ExpectedParentBlock, PayloadFixtureMetadata, PayloadFixtureWriter},
    helpers::fmt_hash,
};
use crate::cli::PreparePayloadArgs;
use arc_eth_engine::{engine::EthereumAPI, rpc::ethereum_rpc::EthereumRPC};
use arc_execution_config::chainspec::ArcChainSpecParser;
use eyre::{bail, Context};
use reth_cli::chainspec::ChainSpecParser;
use std::time::Duration;
use tracing::info;

pub async fn run(args: PreparePayloadArgs) -> eyre::Result<()> {
    if args.from > args.to {
        bail!("--from must be less than or equal to --to");
    }
    if args.batch_size == 0 {
        bail!("--batch-size must be greater than 0");
    }

    let chain_spec = ArcChainSpecParser::parse(&args.chain)
        .wrap_err_with(|| format!("failed to parse chain spec: {}", args.chain))?;
    let genesis = chain_spec.inner.genesis.clone();

    info!(
        chain = args.chain,
        chain_id = genesis.config.chain_id,
        source_rpc_url = args.source_rpc_url,
        output_dir = %args.output_dir.display(),
        from_block = args.from,
        to_block = args.to,
        "preparing payload fixture"
    );

    let source_rpc = ethereum_rpc_client(
        &args.source_rpc_url,
        "source rpc",
        Duration::from_millis(args.eth_rpc_timeout_ms),
    )?;
    let expected_parent = fetch_expected_parent(&source_rpc, args.from).await?;
    let payload_count = write_payload_fixture(
        &source_rpc,
        &args.output_dir,
        &expected_parent,
        &genesis,
        args.from,
        args.to,
        args.batch_size,
    )
    .await?;

    info!(
        output_dir = %args.output_dir.display(),
        payload_count,
        "payload fixture prepared"
    );

    Ok(())
}

async fn fetch_expected_parent(
    source_rpc: &EthereumRPC,
    from_block: u64,
) -> eyre::Result<ExpectedParentBlock> {
    let expected_parent_block_number = from_block
        .checked_sub(1)
        .ok_or_else(|| eyre::eyre!("from_block must be greater than 0"))?;
    let expected_parent_block = source_rpc
        .get_block_by_number(&format!("0x{expected_parent_block_number:x}"))
        .await
        .wrap_err_with(|| {
            format!("failed to fetch source parent block {expected_parent_block_number}")
        })?
        .ok_or_else(|| {
            eyre::eyre!("source parent block {expected_parent_block_number} not found")
        })?;

    Ok(ExpectedParentBlock {
        block_number: expected_parent_block.block_number,
        block_hash: expected_parent_block.block_hash,
    })
}

async fn write_payload_fixture(
    source_rpc: &EthereumRPC,
    output_dir: &std::path::Path,
    expected_parent: &ExpectedParentBlock,
    genesis: &alloy_genesis::Genesis,
    from: u64,
    to: u64,
    batch_size: usize,
) -> eyre::Result<u64> {
    let mut writer = PayloadFixtureWriter::new(output_dir)?;
    let mut payload_count = 0_u64;
    let mut expected_parent_hash = expected_parent.block_hash;

    for chunk_start in (from..=to).step_by(batch_size) {
        let chunk_end = chunk_start
            .saturating_add(batch_size as u64)
            .saturating_sub(1)
            .min(to);
        let block_numbers = (chunk_start..=chunk_end)
            .map(|block_number| format!("0x{block_number:x}"))
            .collect::<Vec<_>>();
        let chunk =
            <EthereumRPC as EthereumAPI>::get_execution_payloads(source_rpc, &block_numbers)
                .await
                .wrap_err_with(|| {
                    format!("failed to fetch source blocks {chunk_start}..={chunk_end}")
                })?;

        for (idx, maybe_payload) in chunk.into_iter().enumerate() {
            let expected_block = chunk_start + idx as u64;
            let payload = maybe_payload
                .ok_or_else(|| eyre::eyre!("source block {expected_block} not found"))?;
            let block = &payload.payload_inner.payload_inner;
            if block.block_number != expected_block {
                bail!(
                    "source payload sequence mismatch: expected block {expected_block}, got {}",
                    block.block_number
                );
            }
            if block.parent_hash != expected_parent_hash {
                bail!(
                    "source payload parent hash mismatch for block {}: expected parent {}, got {}",
                    block.block_number,
                    fmt_hash(expected_parent_hash),
                    fmt_hash(block.parent_hash),
                );
            }

            writer.write_payload(&payload)?;
            expected_parent_hash = block.block_hash;
            payload_count = payload_count.saturating_add(1);
        }
    }

    let metadata = PayloadFixtureMetadata {
        from_block: from,
        to_block: to,
        payload_count,
        expected_parent: expected_parent.clone(),
    };
    writer.finish(&metadata, genesis)?;

    Ok(payload_count)
}
