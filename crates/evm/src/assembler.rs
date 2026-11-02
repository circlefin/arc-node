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

extern crate alloc;

use alloc::sync::Arc;
use alloy_consensus::Block;
use alloy_evm::{block::BlockExecutorFactory, eth::EthBlockExecutionCtx};
use alloy_primitives::{Bytes, U256};
use arc_execution_config::{chainspec::ArcChainSpec, gas_fee::encode_base_fee_to_bytes};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_ethereum::evm::EthBlockAssembler;
use reth_ethereum_primitives::{Receipt, TransactionSigned};
use reth_evm::execute::{BlockAssembler, BlockAssemblerInput, BlockExecutionError};
use revm::context::Block as RevmBlockContext;
use revm_primitives::B256;

use arc_precompiles::system_accounting::{
    compute_gas_values_storage_slot, unpack_gas_values_from_storage, SYSTEM_ACCOUNTING_ADDRESS,
};

const ERR_BLOCK_NUMBER_CONVERSION_FAILED: &str = "Failed to convert block number to u64";

#[derive(Debug, Clone)]
pub struct ArcBlockAssembler<ChainSpec = ArcChainSpec> {
    chain_spec: Arc<ChainSpec>,
}

impl<ChainSpec> ArcBlockAssembler<ChainSpec> {
    pub fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self { chain_spec }
    }
}

/// Encode `nextBaseFee` into header extra_data from the bundle or provider.
/// `Some` uses the bundle slot; `None` falls back to the provider (`simulate`/
/// `eth_call`, or an unchanged ring-buffer slot omitted from the diff). A zero
/// `nextBaseFee` or a missing provider slot leaves extra_data empty. Provider
/// errors fail assembly.
fn extra_data_from_gas_values<E>(
    block_number: u64,
    bundle_slot: Option<U256>,
    provider_read: impl FnOnce() -> Result<Option<U256>, E>,
) -> Result<Bytes, BlockExecutionError>
where
    E: core::error::Error + Send + Sync + 'static,
{
    let value = match bundle_slot {
        Some(value) => value,
        None => {
            let value = provider_read().map_err(|error| {
                tracing::error!(
                    error = %error,
                    block_number,
                    "SystemAccounting storage read failed while assembling extra_data"
                );
                BlockExecutionError::other(error)
            })?;
            match value {
                Some(value) => value,
                None => return Ok(Bytes::new()),
            }
        }
    };

    let gas_values = unpack_gas_values_from_storage(B256::from(value));
    if gas_values.nextBaseFee == 0 {
        tracing::warn!(
            block_number,
            "nextBaseFee is 0 while assembling extra_data; leaving extra_data empty"
        );
        return Ok(Bytes::new());
    }
    Ok(encode_base_fee_to_bytes(gas_values.nextBaseFee))
}

impl<F, ChainSpec> BlockAssembler<F> for ArcBlockAssembler<ChainSpec>
where
    F: for<'a> BlockExecutorFactory<
        ExecutionCtx<'a> = EthBlockExecutionCtx<'a>,
        Transaction = TransactionSigned,
        Receipt = Receipt,
    >,
    ChainSpec: EthChainSpec + EthereumHardforks,
{
    type Block = Block<TransactionSigned>;

    fn assemble_block(
        &self,
        mut input: BlockAssemblerInput<'_, '_, F, <Self::Block as reth_node_api::Block>::Header>,
    ) -> Result<Self::Block, BlockExecutionError> {
        let assembler = EthBlockAssembler::new(self.chain_spec.clone());

        let block_number: u64 = input
            .evm_env
            .block_env()
            .number()
            .try_into()
            .map_err(|err| {
                tracing::error!(error = %err, "Failed to convert block number to u64");
                BlockExecutionError::msg(ERR_BLOCK_NUMBER_CONVERSION_FAILED)
            })?;

        let slot = compute_gas_values_storage_slot(block_number);
        let bundle_slot = input
            .bundle_state
            .account(&SYSTEM_ACCOUNTING_ADDRESS)
            .and_then(|account| account.storage_slot(slot.into()));

        input.execution_ctx.extra_data =
            extra_data_from_gas_values(block_number, bundle_slot, || {
                input
                    .state_provider
                    .storage(SYSTEM_ACCOUNTING_ADDRESS, slot)
            })?;

        assembler.assemble_block(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use arc_execution_config::chainspec::LOCAL_DEV;
    use thiserror::Error;

    #[derive(Debug, Error)]
    #[error("provider I/O failed")]
    struct ProviderIoError;

    fn pack_gas_values_slot(gas_used: u64, gas_used_smoothed: u64, next_base_fee: u64) -> U256 {
        let mut slot = [0u8; 32];
        slot[24..32].copy_from_slice(&gas_used.to_be_bytes());
        slot[16..24].copy_from_slice(&gas_used_smoothed.to_be_bytes());
        slot[8..16].copy_from_slice(&next_base_fee.to_be_bytes());
        U256::from_be_slice(&slot)
    }

    fn unused_provider() -> Result<Option<U256>, ProviderIoError> {
        panic!("provider should not be called when the bundle slot is present");
    }

    #[test]
    fn block_assembler_creation() {
        let chain_spec = LOCAL_DEV.clone();
        let assembler = ArcBlockAssembler::new(chain_spec.clone());

        // Verify the inner assembler's chain_spec points to the same
        assert!(Arc::ptr_eq(&assembler.chain_spec, &chain_spec));
    }

    #[test]
    fn extra_data_uses_bundle_slot_without_provider() {
        let extra =
            extra_data_from_gas_values(1, Some(pack_gas_values_slot(10, 11, 42)), unused_provider)
                .expect("bundle slot must encode extra_data");
        assert_eq!(extra, encode_base_fee_to_bytes(42));
    }

    #[test]
    fn extra_data_stays_empty_when_next_base_fee_is_zero() {
        let extra =
            extra_data_from_gas_values(1, Some(pack_gas_values_slot(0, 0, 0)), unused_provider)
                .expect("zero nextBaseFee must still assemble");
        assert!(extra.is_empty());
    }

    #[test]
    fn extra_data_falls_back_to_provider_slot() {
        let extra = extra_data_from_gas_values(7, None, || {
            Ok::<_, ProviderIoError>(Some(pack_gas_values_slot(1, 2, 99)))
        })
        .expect("provider slot must encode extra_data");
        assert_eq!(extra, encode_base_fee_to_bytes(99));
    }

    #[test]
    fn extra_data_propagates_provider_error() {
        let err = extra_data_from_gas_values(3, None, || Err(ProviderIoError))
            .expect_err("provider error must fail assembly");
        let msg = err.to_string();
        assert!(
            msg.contains("provider I/O failed"),
            "assembly error should preserve the provider error, got {msg}"
        );
    }

    #[test]
    fn extra_data_stays_empty_when_slot_absent() {
        let extra = extra_data_from_gas_values(4, None, || Ok::<_, ProviderIoError>(None))
            .expect("absent slot must still assemble");
        assert!(extra.is_empty());
    }
}
