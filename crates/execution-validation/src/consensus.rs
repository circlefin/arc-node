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

use alloy_primitives::{Bloom, B256};
use reth_chainspec::{EthChainSpec, EthereumHardforks, Hardforks};
use reth_consensus::{Consensus, ConsensusError, FullConsensus, HeaderValidator};
use reth_consensus_common::validation::{
    validate_against_parent_4844, validate_against_parent_hash_number,
    validate_block_pre_execution, validate_body_against_header, validate_header_base_fee,
    validate_header_gas,
};
use reth_ethereum::primitives::{NodePrimitives, RecoveredBlock, SealedHeader};
use reth_ethereum::provider::BlockExecutionResult;
use reth_primitives_traits::{Block, BlockHeader, GotExpected, SealedBlock};
use std::sync::Arc;

use arc_execution_config::chainspec::{BaseFeeConfigProvider, BlockGasLimitProvider};
use arc_execution_config::gas_fee::decode_base_fee_from_bytes;
use arc_execution_config::hardforks::ArcHardfork;

/// Arc Network custom consensus implementation
#[derive(Debug, Clone)]
pub struct ArcConsensus<ChainSpec> {
    chain_spec: Arc<ChainSpec>,
}

impl<ChainSpec> ArcConsensus<ChainSpec> {
    pub const fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self { chain_spec }
    }
}

// Implement HeaderValidator trait
impl<ChainSpec, H> HeaderValidator<H> for ArcConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec
        + EthereumHardforks
        + Hardforks
        + BlockGasLimitProvider
        + BaseFeeConfigProvider,
    H: BlockHeader,
{
    fn validate_header(&self, header: &SealedHeader<H>) -> Result<(), ConsensusError> {
        // Perform standard header validation (gas limits, etc.)
        validate_header_gas(header.header())?;

        // Validate header timestamp (independent of parent)
        arc_validate_header_timestamp(header.header())?;

        // ADR-0003: Validate gas limit is within chainspec bounds (Zero5+)
        arc_validate_gas_limit_bounds(header.header(), &self.chain_spec)?;

        // ADR-0004: extra_data must be exactly 8 bytes (Zero5+).
        arc_validate_extra_data_format(header.header(), &self.chain_spec)?;

        // ADR-0004: base_fee_per_gas must be present (EIP-1559) and within absolute bounds (Zero5+).
        arc_validate_header_base_fee(header.header(), &self.chain_spec)?;

        Ok(())
    }

    // Perform all standard validations
    // reference implementation: https://github.com/paradigmxyz/reth/blob/v1.9.1/crates/ethereum/consensus/src/lib.rs#L170
    fn validate_header_against_parent(
        &self,
        header: &SealedHeader<H>,
        parent: &SealedHeader<H>,
    ) -> Result<(), ConsensusError> {
        // 1. Validate hash and number consistency
        validate_against_parent_hash_number(header.header(), parent)?;

        // 2. Validate timestamp progression
        arc_validate_against_parent_timestamp(header.header(), parent.header())?;

        // 3. Validate base fee using Arc's algorithm
        arc_validate_against_parent_base_fee(header.header(), parent.header(), &self.chain_spec)?;

        // 4. Validate blob gas fields if applicable (EIP-4844)
        if let Some(blob_params) = self.chain_spec.blob_params_at_timestamp(header.timestamp()) {
            validate_against_parent_4844(header.header(), parent.header(), blob_params)?;
        }

        Ok(())
    }
}

/// Validates the timestamp against the parent.
///
/// NOTE(arc): Because Arc is expected to produce blocks at subsecond interval,
/// and because the resolution of the timestamp is in seconds, we must allow
/// the header's timestamp to be equal to its parent's timestamp.
#[inline]
pub fn arc_validate_against_parent_timestamp<H: BlockHeader>(
    header: &H,
    parent: &H,
) -> Result<(), ConsensusError> {
    if header.timestamp() < parent.timestamp() {
        return Err(ConsensusError::TimestampIsInPast {
            parent_timestamp: parent.timestamp(),
            timestamp: header.timestamp(),
        });
    }

    Ok(())
}

/// Validates the base fee against the parent using Arc's custom EMA calculation.
///
/// Decodes `nextBaseFee` from the parent's `extra_data` (written by the executor) and
/// requires the child's `base_fee_per_gas` to match exactly. If `extra_data` is not
/// exactly 8 bytes (e.g. pre-Zero4 blocks), validation is silently skipped.
///
/// Validation is always skipped when the parent is genesis (block 0).
#[inline]
pub fn arc_validate_against_parent_base_fee<ChainSpec, H>(
    header: &H,
    parent: &H,
    chain_spec: &ChainSpec,
) -> Result<(), ConsensusError>
where
    ChainSpec: EthChainSpec + EthereumHardforks + Hardforks,
    H: BlockHeader,
{
    // Skip validation if parent is genesis block
    if parent.number() == 0 {
        return Ok(());
    }

    // Decode the expected base fee from parent's extra_data
    let Some(expected_base_fee) = decode_base_fee_from_bytes(parent.extra_data()) else {
        // Post-Zero5 this branch should be unreachable: `arc_validate_extra_data_format`
        // enforces that extra_data is exactly 8 bytes.
        if chain_spec.is_fork_active_at_block(ArcHardfork::Zero5, parent.number()) {
            tracing::error!(
                parent_number = parent.number(),
                extra_data_len = parent.extra_data().len(),
                "Unexpectedly skipped base fee validation"
            );
        }
        return Ok(());
    };

    // Get the actual base fee from the current header
    let actual_base_fee = header
        .base_fee_per_gas()
        .ok_or(ConsensusError::BaseFeeMissing)?;

    // Verify they match
    if expected_base_fee != actual_base_fee {
        return Err(ConsensusError::BaseFeeDiff(GotExpected {
            got: actual_base_fee,
            expected: expected_base_fee,
        }));
    }

    Ok(())
}

/// Validates that the header's `extra_data` is exactly 8 bytes
///
/// Post-Zero5, the executor always writes `nextBaseFee` as an 8-byte big-endian u64.
/// Any other length is malformed and should be rejected.
#[inline]
fn arc_validate_extra_data_format<H: BlockHeader, CS: Hardforks>(
    header: &H,
    chain_spec: &CS,
) -> Result<(), ConsensusError> {
    if !chain_spec.is_fork_active_at_block(ArcHardfork::Zero5, header.number()) {
        return Ok(());
    }

    let len = header.extra_data().len();
    if len != 8 {
        return Err(ConsensusError::Other(format!(
            "invalid extra_data length {len}: must be 8 bytes"
        )));
    }

    Ok(())
}

/// Validates the header's `base_fee_per_gas`.
///
/// Performs the standard EIP-1559 presence check (via Reth's `validate_header_base_fee`) and,
/// post-Zero5, additionally enforces that the value lies within the chainspec's absolute bounds
/// `[absolute_min_base_fee, absolute_max_base_fee]`.
#[inline]
fn arc_validate_header_base_fee<
    H: BlockHeader,
    CS: EthChainSpec + EthereumHardforks + Hardforks + BaseFeeConfigProvider,
>(
    header: &H,
    chain_spec: &CS,
) -> Result<(), ConsensusError> {
    // Standard EIP-1559 base_fee_per_gas presence check.
    validate_header_base_fee(header, chain_spec)?;

    // Post-Zero5: enforce absolute bounds.
    if !chain_spec.is_fork_active_at_block(ArcHardfork::Zero5, header.number()) {
        return Ok(());
    }

    let base_fee = match header.base_fee_per_gas() {
        Some(fee) => fee,
        None => return Ok(()), // already caught by validate_header_base_fee above
    };

    let config = chain_spec.base_fee_config(header.number());
    let clamped = base_fee.clamp(config.absolute_min_base_fee, config.absolute_max_base_fee);
    if base_fee != clamped {
        return Err(ConsensusError::BaseFeeDiff(GotExpected {
            got: base_fee,
            expected: clamped,
        }));
    }

    Ok(())
}

/// Validates that the header's gas limit is within the chainspec bounds.
///
/// This validation is only active when the Zero5 hardfork is enabled.
#[inline]
fn arc_validate_gas_limit_bounds<H: BlockHeader, CS: Hardforks + BlockGasLimitProvider>(
    header: &H,
    chain_spec: &CS,
) -> Result<(), ConsensusError> {
    if !chain_spec.is_fork_active_at_block(ArcHardfork::Zero5, header.number()) {
        return Ok(());
    }

    let gas_limit = header.gas_limit();
    let config = chain_spec.block_gas_limit_config(header.number());

    if gas_limit < config.min() || gas_limit > config.max() {
        return Err(ConsensusError::Other(format!(
            "block gas limit {gas_limit} outside allowed bounds [{}, {}]",
            config.min(),
            config.max()
        )));
    }

    Ok(())
}

/// The maximum allowed clock skew for Arc proposers, in seconds.
const ARC_PROPOSER_CLOCK_SKEW_THRESHOLD: u64 = 30; // 30 seconds

/// Validates that the header's timestamp is not too far in the future
/// compared to the local system time.
#[inline]
fn arc_validate_header_timestamp<H: BlockHeader>(header: &H) -> Result<(), ConsensusError> {
    // Get the current local time in seconds since UNIX EPOCH
    let local_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ConsensusError::Other("System time is before UNIX EPOCH".to_string()))?
        .as_secs();

    arc_validate_header_timestamp_with_time(header, local_time)
}

/// Validates that the header's timestamp is not too far in the future
/// compared to the local system time.
#[inline]
fn arc_validate_header_timestamp_with_time<H: BlockHeader>(
    header: &H,
    local_time: u64,
) -> Result<(), ConsensusError> {
    // Validate that the header's timestamp is not too far in the future
    if header.timestamp() > local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD {
        return Err(ConsensusError::TimestampIsInFuture {
            timestamp: header.timestamp(),
            present_timestamp: local_time,
        });
    }

    Ok(())
}

// Implement Consensus trait
impl<ChainSpec, B> Consensus<B> for ArcConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec
        + EthereumHardforks
        + Hardforks
        + BlockGasLimitProvider
        + BaseFeeConfigProvider,
    B: Block,
{
    fn validate_body_against_header(
        &self,
        body: &B::Body,
        header: &SealedHeader<B::Header>,
    ) -> Result<(), ConsensusError> {
        // Perform standard body validation (transaction root, etc.)
        validate_body_against_header(body, header.header())
    }

    // Use the standard pre-execution validation from reth
    fn validate_block_pre_execution(&self, block: &SealedBlock<B>) -> Result<(), ConsensusError> {
        validate_block_pre_execution(block, self.chain_spec.as_ref())
    }
}

impl<ChainSpec, N> FullConsensus<N> for ArcConsensus<ChainSpec>
where
    ChainSpec: EthChainSpec
        + EthereumHardforks
        + Hardforks
        + BlockGasLimitProvider
        + BaseFeeConfigProvider,
    N: NodePrimitives,
{
    fn validate_block_post_execution(
        &self,
        block: &RecoveredBlock<N::Block>,
        receipts: &BlockExecutionResult<N::Receipt>,
        receipt_root_bloom: Option<(B256, Bloom)>,
    ) -> Result<(), ConsensusError> {
        reth_ethereum::consensus::validate_block_post_execution(
            block,
            self.chain_spec.as_ref(),
            &receipts.receipts,
            &receipts.requests,
            receipt_root_bloom,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_eips::eip7685::Requests;
    use alloy_primitives::{Bloom, B256};
    use arc_execution_config::chainspec::{
        localdev_with_hardforks, ArcChainSpec, BlockGasLimitProvider, LOCAL_DEV,
    };
    use arc_execution_config::gas_fee::encode_base_fee_to_bytes;
    use reth_chainspec::{ChainSpecBuilder, ForkCondition};
    use reth_ethereum::primitives::Header;
    use reth_ethereum_primitives::{EthPrimitives, Receipt};

    #[test]
    fn test_circle_consensus_creation() {
        let chain_spec = Arc::new(ArcChainSpec::new(ChainSpecBuilder::mainnet().build()));
        let _consensus = ArcConsensus::new(chain_spec);
    }

    #[test]
    fn test_consensus_validates_base_fee_with_ema() {
        let chain_spec = LOCAL_DEV.clone();
        let consensus = ArcConsensus::new(chain_spec);

        let expected_base_fee = 160_000_000_000u64;
        let parent_header = reth_ethereum::primitives::Header {
            number: 10, // Use a non-genesis block number where Zero4 is active
            timestamp: 1000,
            base_fee_per_gas: Some(150_000_000_000),
            extra_data: encode_base_fee_to_bytes(expected_base_fee),
            blob_gas_used: Some(0),
            excess_blob_gas: Some(0),
            ..Default::default()
        };

        let sealed_parent = SealedHeader::new(parent_header.clone(), parent_header.hash_slow());

        let child_header = reth_ethereum::primitives::Header {
            number: 11,
            timestamp: 1001,
            parent_hash: sealed_parent.hash(),
            base_fee_per_gas: Some(expected_base_fee),
            blob_gas_used: Some(0),
            excess_blob_gas: Some(0),
            ..Default::default()
        };

        let sealed_child = SealedHeader::new(child_header.clone(), child_header.hash_slow());

        let result = consensus.validate_header_against_parent(&sealed_child, &sealed_parent);

        assert!(
            result.is_ok(),
            "Should validate base fee using EMA from parent extra_data, but failed: {result:?}"
        );
    }

    #[test]
    fn test_equal_timestamp_is_valid() {
        let chain_spec = Arc::new(ArcChainSpec::new(ChainSpecBuilder::mainnet().build()));
        let consensus = ArcConsensus::new(chain_spec);

        let parent_header = reth_ethereum::primitives::Header {
            timestamp: 1000,
            number: 1,
            ..Default::default()
        };

        let sealed_parent = SealedHeader::new(parent_header.clone(), B256::ZERO);

        let mut child_header = parent_header;
        child_header.number += 1;
        child_header.parent_hash = sealed_parent.hash();

        // Set same timestamp
        child_header.timestamp = 1000;
        let sealed_child = SealedHeader::new(child_header, B256::ZERO);

        let result = consensus.validate_header_against_parent(&sealed_child, &sealed_parent);

        assert!(
            result.is_ok(),
            "Blocks with equal timestamps should be valid, but got: {result:?}"
        );
    }

    #[test]
    fn test_past_timestamp_is_invalid() {
        let chain_spec = Arc::new(ArcChainSpec::new(ChainSpecBuilder::mainnet().build()));
        let consensus = ArcConsensus::new(chain_spec);

        let parent_header = reth_ethereum::primitives::Header {
            timestamp: 1000,
            number: 1,
            ..Default::default()
        };

        let sealed_parent = SealedHeader::new(parent_header.clone(), B256::ZERO);

        let mut child_header = parent_header;
        child_header.number += 1;
        child_header.parent_hash = sealed_parent.hash();

        // Set past timestamp
        child_header.timestamp = 999;
        let sealed_child = SealedHeader::new(child_header, B256::ZERO);

        let result = consensus.validate_header_against_parent(&sealed_child, &sealed_parent);

        assert!(matches!(
            result,
            Err(ConsensusError::TimestampIsInPast { .. })
        ));
    }

    #[test]
    fn test_header_timestamp_just_within_skew_is_valid() {
        let local_time = 1_730_887_500; // Some time in 2024
        let header = Header {
            timestamp: local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD,
            ..Header::default()
        };

        let result = arc_validate_header_timestamp_with_time(&header, local_time);
        assert!(
            result.is_ok(),
            "Timestamp at the exact skew threshold should be valid"
        );
    }

    #[test]
    fn test_header_timestamp_in_past_is_valid() {
        let local_time = 1_730_887_500;
        let header = Header {
            timestamp: local_time - 100,
            ..Header::default()
        };

        let result = arc_validate_header_timestamp_with_time(&header, local_time);
        assert!(
            result.is_ok(),
            "Timestamp in the past should be valid, as this check is only for future timestamps"
        );
    }

    #[test]
    fn test_header_timestamp_too_far_in_future_is_invalid() {
        let local_time = 1_730_887_500;
        let header = Header {
            timestamp: local_time + ARC_PROPOSER_CLOCK_SKEW_THRESHOLD + 10,
            ..Header::default()
        };

        let result = arc_validate_header_timestamp_with_time(&header, local_time);
        assert!(
            matches!(
                result,
                Err(ConsensusError::TimestampIsInFuture {
                    timestamp,
                    present_timestamp,
                }) if timestamp == header.timestamp && present_timestamp == local_time
            ),
            "Timestamp beyond the skew threshold should be invalid"
        );
    }

    #[test]
    fn test_header_timestamp_at_current_time_is_valid() {
        let local_time = 1_730_887_500;
        let header = Header {
            timestamp: local_time,
            ..Header::default()
        };

        let result = arc_validate_header_timestamp_with_time(&header, local_time);
        assert!(
            result.is_ok(),
            "Timestamp equal to local time should be valid"
        );
    }

    #[test]
    fn test_arc_base_fee_validation_with_matching_fees() {
        let chain_spec = LOCAL_DEV.clone();
        let expected_base_fee = 160_000_000_000u64;

        let parent_header = reth_ethereum::primitives::Header {
            number: 10,
            timestamp: 1000,
            base_fee_per_gas: Some(150_000_000_000),
            extra_data: encode_base_fee_to_bytes(expected_base_fee),
            ..Default::default()
        };

        let child_header = reth_ethereum::primitives::Header {
            number: 11,
            timestamp: 1001,
            base_fee_per_gas: Some(expected_base_fee),
            ..Default::default()
        };

        let result =
            arc_validate_against_parent_base_fee(&child_header, &parent_header, &chain_spec);
        assert!(
            result.is_ok(),
            "Base fee validation should succeed when fees match: {result:?}"
        );
    }

    #[test]
    fn test_arc_base_fee_validation_with_mismatched_fees() {
        let chain_spec = LOCAL_DEV.clone();
        let expected_base_fee = 160_000_000_000u64;

        let parent_header = reth_ethereum::primitives::Header {
            number: 10,
            timestamp: 1000,
            base_fee_per_gas: Some(150_000_000_000),
            extra_data: encode_base_fee_to_bytes(expected_base_fee),
            ..Default::default()
        };

        let child_header = reth_ethereum::primitives::Header {
            number: 11,
            timestamp: 1001,
            base_fee_per_gas: Some(170_000_000_000), // Wrong - doesn't match encoded value
            ..Default::default()
        };

        let result =
            arc_validate_against_parent_base_fee(&child_header, &parent_header, &chain_spec);
        assert!(
            matches!(result, Err(ConsensusError::BaseFeeDiff(_))),
            "Base fee validation should fail when fees mismatch: {result:?}"
        );
    }

    #[test]
    fn test_arc_base_fee_validation_skips_malformed_extra_data() {
        use alloy_primitives::Bytes;

        let chain_spec = LOCAL_DEV.clone();

        let parent_header = reth_ethereum::primitives::Header {
            number: 10,
            timestamp: 1000,
            base_fee_per_gas: Some(150_000_000_000),
            extra_data: Bytes::from_static(&[0xff, 0xff]), // Invalid: not 8 bytes
            ..Default::default()
        };

        let child_header = reth_ethereum::primitives::Header {
            number: 11,
            timestamp: 1001,
            base_fee_per_gas: Some(160_000_000_000),
            ..Default::default()
        };

        let result =
            arc_validate_against_parent_base_fee(&child_header, &parent_header, &chain_spec);
        assert!(
            result.is_ok(),
            "Base fee validation should skip when extra_data is malformed: {result:?}"
        );
    }

    #[test]
    fn test_arc_base_fee_validation_missing_base_fee() {
        let chain_spec = LOCAL_DEV.clone();

        let parent_header = reth_ethereum::primitives::Header {
            number: 10,
            timestamp: 1000,
            base_fee_per_gas: Some(150_000_000_000),
            extra_data: encode_base_fee_to_bytes(160_000_000_000),
            ..Default::default()
        };

        let child_header = reth_ethereum::primitives::Header {
            number: 11,
            timestamp: 1001,
            base_fee_per_gas: None, // Missing base fee
            ..Default::default()
        };

        let result =
            arc_validate_against_parent_base_fee(&child_header, &parent_header, &chain_spec);
        assert!(
            matches!(result, Err(ConsensusError::BaseFeeMissing)),
            "Base fee validation should fail when base_fee_per_gas is None: {result:?}"
        );
    }

    #[test]
    fn test_arc_base_fee_validation_skips_genesis_parent() {
        let chain_spec = LOCAL_DEV.clone();

        // Parent is genesis (block 0)
        let parent_header = reth_ethereum::primitives::Header {
            number: 0,
            timestamp: 1000,
            base_fee_per_gas: Some(1000000000),
            extra_data: Default::default(), // Genesis has no encoded base fee
            ..Default::default()
        };

        // Child is block 1
        let child_header = reth_ethereum::primitives::Header {
            number: 1,
            timestamp: 1001,
            base_fee_per_gas: Some(1000000000),
            ..Default::default()
        };

        // Should pass because parent is genesis block
        let result =
            arc_validate_against_parent_base_fee(&child_header, &parent_header, &chain_spec);
        assert!(
            result.is_ok(),
            "Validation should skip when parent is genesis block: {result:?}"
        );
    }

    /// Helper to call validate_block_post_execution and satisfy type constraints.
    fn run_validate_block_post_execution(
        consensus: &ArcConsensus<arc_execution_config::chainspec::ArcChainSpec>,
        block: &reth_primitives_traits::RecoveredBlock<reth_ethereum_primitives::Block>,
        result: &BlockExecutionResult<Receipt>,
    ) -> Result<(), ConsensusError> {
        use reth_consensus::FullConsensus;
        FullConsensus::<EthPrimitives>::validate_block_post_execution(
            consensus, block, result, None,
        )
    }

    /// Builds a block and receipts for post-execution validation testing.
    /// Pass `None` for override values to use the computed value from receipts.
    fn build_post_execution_block(
        receipt_gas: u64,
        logs: Vec<alloy_primitives::Log>,
        override_header_gas: Option<u64>,
        override_receipts_root: Option<alloy_primitives::B256>,
        override_logs_bloom: Option<alloy_primitives::Bloom>,
    ) -> (
        reth_primitives_traits::RecoveredBlock<reth_ethereum_primitives::Block>,
        Vec<reth_ethereum_primitives::Receipt>,
    ) {
        use alloy_consensus::{proofs::calculate_receipt_root, BlockBody, TxReceipt};
        use alloy_eips::eip7685::EMPTY_REQUESTS_HASH;
        use reth_ethereum_primitives::{Block, Receipt, TransactionSigned};
        use reth_primitives_traits::RecoveredBlock;

        let receipts = vec![Receipt {
            tx_type: alloy_consensus::TxType::Legacy,
            success: true,
            cumulative_gas_used: receipt_gas,
            logs,
        }];

        let receipts_with_bloom: Vec<_> = receipts.iter().map(|r| r.with_bloom_ref()).collect();
        let computed_root = calculate_receipt_root(&receipts_with_bloom);
        let computed_bloom = receipts_with_bloom
            .iter()
            .fold(Bloom::ZERO, |bloom, r| bloom | r.bloom_ref());

        let header = reth_ethereum::primitives::Header {
            number: 100,
            timestamp: 1700000000,
            gas_used: override_header_gas.unwrap_or(receipt_gas),
            receipts_root: override_receipts_root.unwrap_or(computed_root),
            logs_bloom: override_logs_bloom.unwrap_or(computed_bloom),
            requests_hash: Some(EMPTY_REQUESTS_HASH),
            ..Default::default()
        };

        let block = RecoveredBlock::new_unhashed(
            Block::new(
                header,
                BlockBody::<TransactionSigned> {
                    transactions: vec![],
                    ommers: vec![],
                    withdrawals: None,
                },
            ),
            vec![],
        );

        (block, receipts)
    }

    #[test]
    fn test_post_execution_validation_success() {
        use alloy_primitives::{Address, Log};

        let log = Log::new_unchecked(
            Address::repeat_byte(0x42),
            vec![B256::repeat_byte(0x01)],
            alloy_primitives::Bytes::from_static(b"test data"),
        );
        let (block, receipts) = build_post_execution_block(50000, vec![log], None, None, None);
        let consensus = ArcConsensus::new(LOCAL_DEV.clone());
        let execution_result = BlockExecutionResult {
            receipts,
            requests: Requests::default(),
            gas_used: block.header().gas_used,
            blob_gas_used: 0,
        };
        let result = run_validate_block_post_execution(&consensus, &block, &execution_result);
        assert!(result.is_ok(), "Expected success: {result:?}");
    }

    #[test]
    fn test_post_execution_gas_used_mismatch() {
        let actual_gas_used = 42000;
        let header_gas_used = 26000;

        let (block, receipts) =
            build_post_execution_block(actual_gas_used, vec![], Some(header_gas_used), None, None);
        let consensus = ArcConsensus::new(LOCAL_DEV.clone());
        let execution_result = BlockExecutionResult {
            receipts,
            requests: Requests::default(),
            gas_used: actual_gas_used,
            blob_gas_used: 0,
        };
        let result = run_validate_block_post_execution(&consensus, &block, &execution_result);
        assert!(
            matches!(result, Err(ConsensusError::BlockGasUsed { .. })),
            "Expected BlockGasUsed error: {result:?}"
        );
    }

    #[test]
    fn test_post_execution_receipts_root_mismatch() {
        // Override receipts root to an arbitrary value
        let (block, receipts) =
            build_post_execution_block(26000, vec![], None, Some(B256::repeat_byte(0xff)), None);
        let consensus = ArcConsensus::new(LOCAL_DEV.clone());
        let execution_result = BlockExecutionResult {
            receipts,
            requests: Requests::default(),
            gas_used: block.header().gas_used,
            blob_gas_used: 0,
        };
        let result = run_validate_block_post_execution(&consensus, &block, &execution_result);
        assert!(
            matches!(result, Err(ConsensusError::BodyReceiptRootDiff(_))),
            "Expected BodyReceiptRootDiff error: {result:?}"
        );
    }

    #[test]
    fn test_post_execution_logs_bloom_mismatch() {
        // Override logs bloom to an arbitrary value
        let (block, receipts) =
            build_post_execution_block(26000, vec![], None, None, Some(Bloom::repeat_byte(0xff)));
        let consensus = ArcConsensus::new(LOCAL_DEV.clone());
        let execution_result = BlockExecutionResult {
            receipts,
            requests: Requests::default(),
            gas_used: block.header().gas_used,
            blob_gas_used: 0,
        };
        let result = run_validate_block_post_execution(&consensus, &block, &execution_result);
        assert!(
            matches!(result, Err(ConsensusError::BodyBloomLogDiff(_))),
            "Expected BodyBloomLogDiff error: {result:?}"
        );
    }

    #[test]
    fn test_gas_limit_within_bounds_is_valid() {
        // LOCAL_DEV has Zero5 active at block 0
        let spec = LOCAL_DEV.clone();
        let config = spec.block_gas_limit_config(1);

        let header = Header {
            number: 1,
            gas_limit: config.default(),
            timestamp: 0,
            ..Default::default()
        };
        let result = arc_validate_gas_limit_bounds(&header, spec.as_ref());
        assert!(
            result.is_ok(),
            "Gas limit within bounds should be valid: {result:?}"
        );
    }

    #[test]
    fn test_gas_limit_below_min_is_invalid() {
        let spec = LOCAL_DEV.clone();
        let config = spec.block_gas_limit_config(1);

        let header = Header {
            number: 1,
            gas_limit: config.min() - 1,
            timestamp: 0,
            ..Default::default()
        };
        let result = arc_validate_gas_limit_bounds(&header, spec.as_ref());
        assert!(
            matches!(result, Err(ConsensusError::Other(_))),
            "Gas limit below min should be invalid: {result:?}"
        );
    }

    #[test]
    fn test_gas_limit_above_max_is_invalid() {
        let spec = LOCAL_DEV.clone();
        let config = spec.block_gas_limit_config(1);

        let header = Header {
            number: 1,
            gas_limit: config.max() + 1,
            timestamp: 0,
            ..Default::default()
        };
        let result = arc_validate_gas_limit_bounds(&header, spec.as_ref());
        assert!(
            matches!(result, Err(ConsensusError::Other(_))),
            "Gas limit above max should be invalid: {result:?}"
        );
    }

    #[test]
    fn test_arc_validate_header_base_fee() {
        // LOCAL_DEV: Zero5 active
        let spec = LOCAL_DEV.clone();
        let config = spec.base_fee_config(1);

        // Within bounds
        for fee in [
            config.absolute_min_base_fee,
            1_000_000_000u64,
            config.absolute_max_base_fee,
        ] {
            let header = Header {
                number: 1,
                base_fee_per_gas: Some(fee),
                ..Default::default()
            };
            assert!(
                arc_validate_header_base_fee(&header, spec.as_ref()).is_ok(),
                "fee={fee}"
            );
        }

        // Out of bounds
        for fee in [
            config.absolute_min_base_fee - 1,
            config.absolute_max_base_fee + 1,
        ] {
            let header = Header {
                number: 1,
                base_fee_per_gas: Some(fee),
                ..Default::default()
            };
            assert!(matches!(
                arc_validate_header_base_fee(&header, spec.as_ref()),
                Err(ConsensusError::BaseFeeDiff(_))
            ));
        }

        // Pre-Zero5: bounds check is skipped entirely.
        let pre_zero5 = localdev_with_hardforks(&[(ArcHardfork::Zero4, 0)]);
        let header = Header {
            number: 1,
            base_fee_per_gas: Some(config.absolute_min_base_fee - 1),
            ..Default::default()
        };
        assert!(arc_validate_header_base_fee(&header, pre_zero5.as_ref()).is_ok());
    }

    #[test]
    fn test_arc_validate_extra_data_format() {
        use alloy_primitives::Bytes;

        // Zero5
        let spec = LOCAL_DEV.clone();

        let valid = Header {
            number: 1,
            extra_data: Bytes::from([0u8; 8].as_slice()),
            ..Default::default()
        };
        assert!(arc_validate_extra_data_format(&valid, spec.as_ref()).is_ok());

        let too_short = Header {
            number: 1,
            extra_data: Bytes::from([0u8; 7].as_slice()),
            ..Default::default()
        };
        assert!(matches!(
            arc_validate_extra_data_format(&too_short, spec.as_ref()),
            Err(ConsensusError::Other(_))
        ));

        let too_long = Header {
            number: 1,
            extra_data: Bytes::from([0u8; 9].as_slice()),
            ..Default::default()
        };
        assert!(matches!(
            arc_validate_extra_data_format(&too_long, spec.as_ref()),
            Err(ConsensusError::Other(_))
        ));

        // Pre-Zero5: length check is skipped entirely.
        let pre_zero5 = localdev_with_hardforks(&[(ArcHardfork::Zero4, 0)]);
        let header = Header {
            number: 1,
            extra_data: Bytes::from([0u8; 7].as_slice()),
            ..Default::default()
        };
        assert!(arc_validate_extra_data_format(&header, pre_zero5.as_ref()).is_ok());
    }

    #[test]
    fn test_gas_limit_validation_skipped_before_zero5() {
        // Create a chain spec where Zero5 activates at block 100
        let mut inner = ChainSpecBuilder::mainnet().build();
        inner
            .hardforks
            .insert(ArcHardfork::Zero5, ForkCondition::Block(100));
        let spec = Arc::new(ArcChainSpec::new(inner));

        // Before Zero5 — any gas limit should pass
        let header = Header {
            number: 99,
            gas_limit: 0, // would be invalid after Zero5
            timestamp: 0,
            ..Default::default()
        };
        let result = arc_validate_gas_limit_bounds(&header, spec.as_ref());
        assert!(
            result.is_ok(),
            "Before Zero5, gas limit validation should be skipped: {result:?}"
        );

        // At Zero5 — now enforced
        let header = Header {
            number: 100,
            gas_limit: 0,
            timestamp: 0,
            ..Default::default()
        };
        let result = arc_validate_gas_limit_bounds(&header, spec.as_ref());
        assert!(
            matches!(result, Err(ConsensusError::Other(_))),
            "At Zero5, gas limit 0 should be invalid: {result:?}"
        );
    }
}
