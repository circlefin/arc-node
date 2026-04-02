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

use alloy_evm::block::ExecutableTx;
use alloy_evm::block::TxResult;
use alloy_evm::eth::receipt_builder::ReceiptBuilder;
use reth_chainspec::EthChainSpec;
use reth_chainspec::Hardforks;
use reth_ethereum::{
    evm::{
        primitives::{
            execute::{BlockExecutionError, BlockExecutor},
            Database, OnStateHook,
        },
        revm::db::State,
    },
    provider::BlockExecutionResult,
};
use reth_evm::eth::EthBlockExecutionCtx;
use revm::{context::Block, context_interface::result::ResultAndState};

use alloy_consensus::transaction::Transaction;
use alloy_consensus::transaction::TransactionEnvelope;
use alloy_consensus::TxReceipt;
use alloy_eips::eip2718::Encodable2718;
use alloy_eips::eip7685::Requests;
use alloy_evm::block::BlockValidationError;
use alloy_evm::block::InternalBlockExecutionError;
use alloy_evm::block::StateChangeSource;
use alloy_evm::block::SystemCaller;
use alloy_evm::eth::receipt_builder::ReceiptBuilderCtx;
use alloy_evm::eth::spec::EthExecutorSpec;
use alloy_evm::Evm;
use alloy_evm::FromRecoveredTx;
use alloy_evm::FromTxWithEncoded;
use alloy_evm::RecoveredTx;
use alloy_primitives::{Address, Log};
use arc_execution_config::chainspec::{BaseFeeConfigProvider, BlockGasLimitProvider};
use arc_execution_config::gas_fee::{
    self, arc_calc_next_block_base_fee, decode_base_fee_from_bytes,
};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_config::native_coin_control::{
    compute_is_blocklisted_storage_slot, is_blocklisted_status,
};
use arc_execution_config::protocol_config;
use arc_precompiles::helpers::ERR_BLOCKED_ADDRESS;
use arc_precompiles::system_accounting;
use reth_evm::block::StateChangePostBlockSource;
use revm::DatabaseCommit;

const ERR_BLOCKLIST_READ_FAILED: &str = "Failed to read beneficiary blocklist status";
const ERR_BENEFICIARY_MISMATCH: &str =
    "Block beneficiary does not match ProtocolConfig.rewardBeneficiary()";
const ERR_BLOCK_NUMBER_CONVERSION_FAILED: &str = "Failed to convert block number to u64";

/// Result of executing an Arc transaction.
#[derive(Debug)]
pub struct ArcTxResult<H, T> {
    /// Result of the transaction execution.
    pub result: ResultAndState<H>,
    /// Blob gas used by the transaction.
    pub blob_gas_used: u64,
    /// Type of the transaction.
    pub tx_type: T,
}

impl<H, T> TxResult for ArcTxResult<H, T> {
    type HaltReason = H;

    fn result(&self) -> &ResultAndState<Self::HaltReason> {
        &self.result
    }
}

/// Custom block executor for Arc
///
/// This functionality is mostly forked from: https://github.com/alloy-rs/evm/blob/v0.23.2/crates/evm/src/eth/block.rs
/// with modifications to support Arc-specific functionality.
pub struct ArcBlockExecutor<'a, Evm, Spec, R: ReceiptBuilder> {
    /// Context for block execution.
    pub ctx: EthBlockExecutionCtx<'a>,
    /// Chain spec.
    chain_spec: Spec,
    /// Inner EVM.
    evm: Evm,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<Spec>,
    /// Receipt builder.
    receipt_builder: R,
    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by transactions in this block.
    gas_used: u64,
    /// Total blob gas used by transactions in this block.
    blob_gas_used: u64,
}

impl<'a, Evm, Spec, R> ArcBlockExecutor<'a, Evm, Spec, R>
where
    Spec: Clone,
    R: ReceiptBuilder,
    Evm: alloy_evm::Evm,
{
    /// Creates a new [`ArcBlockExecutor`]
    pub fn new(evm: Evm, ctx: EthBlockExecutionCtx<'a>, spec: Spec, receipt_builder: R) -> Self {
        Self {
            chain_spec: spec.clone(),
            evm,
            ctx,
            receipts: Vec::new(),
            gas_used: 0,
            blob_gas_used: 0,
            system_caller: SystemCaller::new(spec.clone()),
            receipt_builder,
        }
    }

    /// Current block number as `u64`.
    fn block_number_u64(&self) -> Result<u64, BlockExecutionError> {
        let block_number = self.evm.block().number();
        block_number.try_into().map_err(|err| {
            tracing::error!(
                error = %err,
                block_number = %block_number,
                "Failed to convert block number to u64"
            );
            BlockExecutionError::msg(ERR_BLOCK_NUMBER_CONVERSION_FAILED)
        })
    }
}

fn validate_expected_beneficiary(
    header_beneficiary: Address,
    expected_beneficiary: Address,
    block_number: u64,
) -> Result<(), BlockExecutionError> {
    if expected_beneficiary.is_zero() || header_beneficiary == expected_beneficiary {
        return Ok(());
    }

    tracing::warn!(
        header_beneficiary = %header_beneficiary,
        expected_beneficiary = %expected_beneficiary,
        block_number = block_number,
        "{}",
        ERR_BENEFICIARY_MISMATCH
    );
    Err(BlockValidationError::msg(ERR_BENEFICIARY_MISMATCH).into())
}

fn validate_beneficiary_not_blocklisted<DB: Database>(
    db: &mut DB,
    header_beneficiary: Address,
    block_number: u64,
) -> Result<(), BlockExecutionError> {
    let is_blocklisted = db
        .storage(
            arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS,
            compute_is_blocklisted_storage_slot(header_beneficiary).into(),
        )
        .map(is_blocklisted_status)
        .map_err(|error| {
            let reason = format!("NativeCoinControl blocklist storage read failed: {error}");
            tracing::error!(
                error = %reason,
                header_beneficiary = %header_beneficiary,
                block_number = block_number,
                "Failed to read blocklist status for proposer-selected beneficiary"
            );
            BlockValidationError::msg(ERR_BLOCKLIST_READ_FAILED)
        })?;

    if is_blocklisted {
        tracing::warn!(
            header_beneficiary = %header_beneficiary,
            block_number = block_number,
            "Proposer selected a blocklisted beneficiary"
        );
        return Err(BlockValidationError::msg(ERR_BLOCKED_ADDRESS).into());
    }

    Ok(())
}

impl<'db, DB, E, Spec, R> ArcBlockExecutor<'_, E, Spec, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    Spec:
        EthExecutorSpec + Hardforks + EthChainSpec + BlockGasLimitProvider + BaseFeeConfigProvider,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    /// Validates that block `extra_data` encodes the same next base fee that this executor
    /// computed for the current block.
    fn validate_extra_data_base_fee(
        &self,
        block_number: u64,
        expected_next_base_fee: u64,
    ) -> Result<(), BlockExecutionError> {
        let extra_data_base_fee = decode_base_fee_from_bytes(&self.ctx.extra_data);
        if extra_data_base_fee != Some(expected_next_base_fee) {
            return Err(BlockExecutionError::Validation(BlockValidationError::Other(
                format!(
                    "extra_data base fee mismatch at block {block_number}: computed nextBaseFee={expected_next_base_fee}, extra_data={extra_data_base_fee:?}"
                )
                .into(),
            )));
        }
        Ok(())
    }

    /// Computes `GasValues` using the pre-Zero5 path
    fn compute_gas_values_legacy(
        &mut self,
        block_number: u64,
        fee_params: Option<protocol_config::IProtocolConfig::FeeParams>,
    ) -> Result<system_accounting::GasValues, BlockExecutionError> {
        let Some(fee_params) = fee_params else {
            return Ok(system_accounting::GasValues {
                gasUsed: self.gas_used,
                gasUsedSmoothed: self.gas_used,
                nextBaseFee: 0,
            });
        };

        let parent_block_number = block_number.saturating_sub(1);
        let parent_gas_values =
            system_accounting::retrieve_gas_values(parent_block_number, &mut self.evm).map_err(
                |e| BlockExecutionError::Internal(InternalBlockExecutionError::Other(Box::new(e))),
            )?;

        let calculated_smoothed_gas_used = gas_fee::determine_ema_parent_gas_used(
            parent_gas_values.gasUsedSmoothed,
            self.gas_used,
            fee_params.alpha,
        );

        let mut next_base_fee: u64 = 0;
        if let Some(smoothed_gas_used) = calculated_smoothed_gas_used {
            let raw = arc_calc_next_block_base_fee(
                smoothed_gas_used,
                self.evm.block().gas_limit(),
                self.evm.block().basefee(),
                fee_params.kRate,
                fee_params.inverseElasticityMultiplier,
            );
            next_base_fee = protocol_config::determine_bounded_base_fee(&fee_params, raw);
        }

        let smoothed_gas_used = calculated_smoothed_gas_used.unwrap_or(self.gas_used);

        Ok(system_accounting::GasValues {
            gasUsed: self.gas_used,
            gasUsedSmoothed: smoothed_gas_used,
            nextBaseFee: next_base_fee,
        })
    }

    /// Computes `GasValues` using the ADR-0004 spec (Zero5+).
    ///
    /// Validates the on-chain `FeeParams` against the chainspec `BaseFeeConfig` bounds,
    /// substituting per-field defaults for any out-of-range value. If ProtocolConfig is
    /// unavailable, falls back to each field's `default`.  Applies EMA smoothing, computes
    /// the next base fee, optionally applies the ProtocolConfig `minBaseFee`/`maxBaseFee`
    /// clamp, then applies the chainspec absolute bounds clamp.
    fn compute_gas_values(
        &mut self,
        block_number: u64,
        fee_params: Option<protocol_config::IProtocolConfig::FeeParams>,
    ) -> Result<system_accounting::GasValues, BlockExecutionError> {
        if fee_params.is_none() {
            tracing::warn!(
                block_number,
                "ProtocolConfig unavailable post-Zero5; computing next_base_fee with chainspec defaults"
            );
        }

        let base_fee_config = self.chain_spec.base_fee_config(block_number + 1);
        let calc = base_fee_config.resolve_calc_params(fee_params.as_ref());

        let parent_block_number = block_number.saturating_sub(1);
        let parent_gas_values =
            system_accounting::retrieve_gas_values(parent_block_number, &mut self.evm).map_err(
                |e| {
                    tracing::warn!(
                        error = %e,
                        block_number,
                        "Failed to retrieve parent gas values from SystemAccounting"
                    );
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(Box::new(e)))
                },
            )?;

        let smoothed_gas_used = gas_fee::determine_ema_parent_gas_used(
            parent_gas_values.gasUsedSmoothed,
            self.gas_used,
            calc.alpha,
        )
        .unwrap_or(self.gas_used);

        let raw_next_base_fee = arc_calc_next_block_base_fee(
            smoothed_gas_used,
            self.evm.block().gas_limit(),
            self.evm.block().basefee(),
            calc.k_rate,
            calc.inverse_elasticity_multiplier,
        );

        // Apply ProtocolConfig's own minBaseFee/maxBaseFee clamp if available.
        let clamped = match fee_params.as_ref() {
            Some(fp) => protocol_config::determine_bounded_base_fee(fp, raw_next_base_fee),
            None => raw_next_base_fee,
        };

        let next_base_fee = base_fee_config.clamp_absolute(clamped);

        Ok(system_accounting::GasValues {
            gasUsed: self.gas_used,
            gasUsedSmoothed: smoothed_gas_used,
            nextBaseFee: next_base_fee,
        })
    }
}

impl<'db, DB, E, Spec, R> BlockExecutor for ArcBlockExecutor<'_, E, Spec, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    Spec:
        EthExecutorSpec + Hardforks + EthChainSpec + BlockGasLimitProvider + BaseFeeConfigProvider,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;
    type Result = ArcTxResult<E::HaltReason, <R::Transaction as TransactionEnvelope>::TxType>;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Spurious Dragon hardfork is enabled
        self.evm.db_mut().set_state_clear_flag(true);

        // For block voter to validate block beneficiary matches ProtocolConfig.rewardBeneficiary()
        // Only validate after Zero5 hardfork when this requirement is active
        let block_number = self.block_number_u64()?;

        if self
            .chain_spec
            .is_fork_active_at_block(ArcHardfork::Zero5, block_number)
        {
            // EIP-2935: persist parent block hash in history storage contract.
            // Internally gates on Prague activation and is a no-op at block 0 (genesis).
            self.system_caller
                .apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;

            if let Ok(expected_beneficiary) = protocol_config::retrieve_reward_beneficiary(&mut self.evm)
                .inspect_err(|error| {
                    // Log and continue to avoid halting the chain (e.g. if ProtocolConfig is deprecated).
                    tracing::warn!(
                        error = %error,
                        block_number = block_number,
                        "Failed to retrieve reward beneficiary from ProtocolConfig, continuing without validation"
                    );
                })
            {
                let header_beneficiary = self.evm.block().beneficiary();

                validate_expected_beneficiary(
                    header_beneficiary,
                    expected_beneficiary,
                    block_number,
                )?;

                // If expected beneficiary is zero address, proposer is allowed to set the beneficiary,
                // but the selected address must not be blocklisted.
                if expected_beneficiary.is_zero() {
                    validate_beneficiary_not_blocklisted(
                        self.evm.db_mut(),
                        header_beneficiary,
                        block_number,
                    )?;
                }
            }

            // ADR-0003: Stateful gas limit validation against ProtocolConfig (Zero5+)
            let block_gas_limit = self.evm.block().gas_limit();
            let fee_params = protocol_config::retrieve_fee_params(&mut self.evm).inspect_err(|err| {
                    tracing::warn!(error = ?err, block_number, "Failed to get fee params from ProtocolConfig for gas limit validation");
                }).ok();

            let gas_limit_config = self.chain_spec.block_gas_limit_config(block_number);
            let expected =
                protocol_config::expected_gas_limit(fee_params.as_ref(), &gas_limit_config);

            if block_gas_limit != expected {
                return Err(BlockExecutionError::Validation(
                    BlockValidationError::Other(
                        format!(
                            "block gas limit {block_gas_limit} does not match expected {expected}"
                        )
                        .into(),
                    ),
                ));
            }
        }

        Ok(())
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::Result, BlockExecutionError> {
        let (tx_env, tx) = tx.into_parts();

        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit() - self.gas_used;

        if tx.tx().gas_limit() > block_available_gas {
            return Err(
                BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                    transaction_gas_limit: tx.tx().gas_limit(),
                    block_available_gas,
                }
                .into(),
            );
        }

        // Execute transaction.
        let result = self
            .evm
            .transact(tx_env)
            .map_err(|err| BlockExecutionError::evm(err, tx.tx().trie_hash()))?;

        Ok(ArcTxResult {
            result,
            blob_gas_used: tx.tx().blob_gas_used().unwrap_or_default(),
            tx_type: tx.tx().tx_type(),
        })
    }

    fn commit_transaction(&mut self, output: Self::Result) -> Result<u64, BlockExecutionError> {
        let ArcTxResult {
            result: ResultAndState { result, state },
            blob_gas_used,
            tx_type,
        } = output;

        self.system_caller
            .on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        // Cancun is always active for arc
        self.blob_gas_used = self.blob_gas_used.saturating_add(blob_gas_used);

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts
            .push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
                tx_type,
                evm: &self.evm,
                result,
                state: &state,
                cumulative_gas_used: self.gas_used,
            }));

        // Commit the state changes.
        self.evm.db_mut().commit(state);

        Ok(gas_used)
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<Self::Receipt>), BlockExecutionError> {
        // EIP-6110 not activated
        let requests = Requests::default();

        let block_number = self.block_number_u64()?;

        // At the end of the block, call a system contract (precompile) to persist gas accounting
        // state: raw gas used, smoothed gas used, and the next block's base fee.
        let fee_params = protocol_config::retrieve_fee_params(&mut self.evm)
            .inspect_err(|e| {
                tracing::error!(
                    error = %e,
                    block_number,
                    "Failed to retrieve fee params from ProtocolConfig"
                );
            })
            .ok();
        let is_zero5 = self
            .chain_spec
            .is_fork_active_at_block(ArcHardfork::Zero5, block_number);
        let gas_values = if is_zero5 {
            // ADR-0004 implementation: compute gas values within local bounds
            self.compute_gas_values(block_number, fee_params)?
        } else {
            self.compute_gas_values_legacy(block_number, fee_params)?
        };

        // ADR-004: enforce extra_data matches what is computed, but only when executing an
        // existing payload (extra_data already set by consensus). During block building the
        // executor writes extra_data itself, so it is empty at this point — skip validation.
        if is_zero5 && !self.ctx.extra_data.is_empty() {
            self.validate_extra_data_base_fee(block_number, gas_values.nextBaseFee)?;
        }

        let state = system_accounting::store_gas_values(block_number, gas_values, &mut self.evm)
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to store gas values to SystemAccounting");
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(Box::new(e)))
            })?;

        self.system_caller.on_state(
            StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
            &state,
        );

        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used: self.gas_used,
                blob_gas_used: self.blob_gas_used,
            },
        ))
    }

    fn receipts(&self) -> &[Self::Receipt] {
        &self.receipts
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    use alloy_genesis::Genesis;
    use alloy_primitives::address;
    use alloy_primitives::map::HashMap;
    use alloy_primitives::B256 as AlloyB256;
    use alloy_primitives::KECCAK256_EMPTY;
    use reth_chainspec::EthChainSpec;
    use reth_evm::ConfigureEvm;
    use reth_evm::EvmEnv;

    use revm::{
        context::{BlockEnv, CfgEnv},
        database::InMemoryDB,
        state::{AccountInfo, Bytecode},
    };
    use revm_primitives::ruint::aliases::U256;
    use revm_primitives::{hardfork::SpecId, keccak256};
    use revm_primitives::{StorageKey, StorageValue};

    use arc_execution_config::chainspec::{
        localdev_with_hardforks, ArcChainSpec, BaseFeeConfigProvider, LOCAL_DEV,
    };

    // Build env from localdev genesis so ProtocolConfig is available
    pub fn insert_alloc_into_db(db: &mut InMemoryDB, genesis: &Genesis) {
        for addr in genesis.alloc.keys() {
            let data = genesis.alloc.get(addr).unwrap().clone();
            match data.code.clone() {
                Some(code) => db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: data.balance,
                        nonce: data.nonce.unwrap_or_default(),
                        code_hash: keccak256(&code),
                        code: Some(Bytecode::new_raw(code)),
                        account_id: None,
                    },
                ),
                None => db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: data.balance,
                        nonce: data.nonce.unwrap_or_default(),
                        code_hash: KECCAK256_EMPTY,
                        code: None,
                        account_id: None,
                    },
                ),
            }
            for (k, v) in data.storage_slots() {
                db.insert_account_storage(*addr, k.into(), v)
                    .expect("insert storage");
            }
        }
    }

    // Input values used only for pre-Zero5 patch_fee_params calls — not used as expected outputs.
    const GENESIS_K_RATE: u64 = 200;
    const GENESIS_INVERSE_ELASTICITY_MULTIPLIER: u64 = 5000;
    const GAS_USED: u64 = 100_000;

    fn get_mock_block_env() -> BlockEnv {
        BlockEnv {
            basefee: 10000,
            gas_limit: 30000000,
            ..Default::default()
        }
    }

    /// Helper function to create a block execution context
    fn get_mock_execution_ctx<'a>() -> reth_evm::eth::EthBlockExecutionCtx<'a> {
        reth_evm::eth::EthBlockExecutionCtx {
            parent_hash: AlloyB256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data: Default::default(),
            tx_count_hint: None,
        }
    }

    /// Helper function to create an ArcEvmConfig
    fn create_evm_config(chain_spec: alloc::sync::Arc<ArcChainSpec>) -> crate::evm::ArcEvmConfig {
        crate::evm::ArcEvmConfig::new(reth_ethereum::evm::EthEvmConfig::new_with_evm_factory(
            chain_spec.clone(),
            crate::evm::ArcEvmFactory::new(chain_spec),
        ))
    }

    /// Helper function to query the expected beneficiary from ProtocolConfig
    fn query_expected_beneficiary(
        chain_spec: alloc::sync::Arc<ArcChainSpec>,
        db: &mut InMemoryDB,
        block_number: u64,
    ) -> Address {
        let evm_config = create_evm_config(chain_spec.clone());

        let mut temp_block_env = get_mock_block_env();
        temp_block_env.number = U256::from(block_number);
        let temp_cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let temp_evm_env = EvmEnv {
            cfg_env: temp_cfg_env,
            block_env: temp_block_env,
        };
        let mut temp_state = State::builder().with_database(db).build();
        let mut temp_evm = evm_config.evm_with_env(&mut temp_state, temp_evm_env);

        protocol_config::retrieve_reward_beneficiary(&mut temp_evm)
            .expect("Should be able to query reward beneficiary")
    }

    /// Patch ProtocolConfig storage so rewardBeneficiary() returns 0x00.
    fn set_beneficiary_to_zero_address(db: &mut InMemoryDB) {
        // ProtocolConfig ERC-7201 base slot from the contract (see ProtocolConfig.sol).
        const PROTOCOL_CONFIG_STORAGE_LOCATION_HEX: &str =
            "668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200";
        let base_slot = U256::from_str_radix(PROTOCOL_CONFIG_STORAGE_LOCATION_HEX, 16).unwrap();
        // rewardBeneficiary is at base slot + 4.
        let reward_beneficiary_slot = base_slot.saturating_add(U256::from(4));

        db.insert_account_storage(
            protocol_config::PROTOCOL_CONFIG_ADDRESS,
            reward_beneficiary_slot,
            StorageValue::from(0u64),
        )
        .expect("Insert storage");
    }

    fn mark_address_as_blocklisted(db: &mut InMemoryDB, beneficiary: Address) {
        let storage_slot = compute_is_blocklisted_storage_slot(beneficiary).into();
        db.insert_account_storage(
            arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS,
            storage_slot,
            StorageValue::from(1u64),
        )
        .expect("Insert storage");
    }

    /// Runs the executor finish() and returns the gas values stored in the precompile
    fn run_executor_finish_and_query_gas_values(
        chain_spec: alloc::sync::Arc<ArcChainSpec>,
        block_env: &BlockEnv,
        db: &mut InMemoryDB,
    ) -> system_accounting::GasValues {
        // Build EVM env manually (mirrors tests/common.rs pattern)
        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv {
            cfg_env,
            block_env: block_env.clone(),
        };

        let evm_config =
            crate::evm::ArcEvmConfig::new(reth_ethereum::evm::EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                crate::evm::ArcEvmFactory::new(chain_spec.clone()),
            ));

        let mut state = reth_ethereum::evm::revm::db::State::builder()
            .with_database(db) // or `state.set_db(db)` depending on your version
            .build();

        let evm = evm_config.evm_with_env(&mut state, evm_env);
        let ctx = reth_evm::eth::EthBlockExecutionCtx {
            parent_hash: AlloyB256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data: Default::default(),
            tx_count_hint: None,
        };

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.clone(),
            evm_config.inner.executor_factory.receipt_builder(),
        );
        executor.gas_used = GAS_USED;

        let (mut evm_after, _result) = executor.finish().expect("finish()");
        let current_block_number = 0u64; // block env default number in our test
        arc_precompiles::system_accounting::retrieve_gas_values(
            current_block_number,
            &mut evm_after,
        )
        .expect("retrieve")
    }

    #[test]
    fn test_executor_stores_smoothed_gas_used_according_to_protocol_config() {
        let block_env = get_mock_block_env();

        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());

        let stored =
            run_executor_finish_and_query_gas_values(chain_spec.clone(), &block_env, &mut db);
        let block_env = get_mock_block_env();

        assert_eq!(stored.gasUsed, GAS_USED);
        let defaults = chain_spec.base_fee_config(1).resolve_calc_params(None);
        let expected_smoothed = GAS_USED * defaults.alpha / 100u64;
        assert_eq!(stored.gasUsedSmoothed, expected_smoothed);
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
    }

    #[test]
    fn test_executor_stores_raw_gas_used_if_protocol_config_is_not_available() {
        let block_env = get_mock_block_env();

        // Brick the protocol config contract by overwriting the implementation slot
        // Implementation slot: 0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc
        fn patch_protocol_config_to_invalid_impl(db: &mut InMemoryDB) {
            db.replace_account_storage(
                address!("3600000000000000000000000000000000000001"),
                HashMap::from_iter([(
                    StorageKey::from_str_radix(
                        "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
                        16,
                    )
                    .unwrap(),
                    StorageValue::from(0u64),
                )]),
            )
            .expect("Replace storage");
        }

        // ADR-0004 (Zero5+): When ProtocolConfig is unavailable, the executor uses
        // each field's default from the chainspec BaseFeeConfig
        let chain_spec = LOCAL_DEV.clone(); // Zero5 active at block 0

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_protocol_config_to_invalid_impl(&mut db);
        let stored =
            run_executor_finish_and_query_gas_values(chain_spec.clone(), &block_env, &mut db);
        assert_eq!(stored.gasUsed, GAS_USED);

        // EMA smoothing and fee calculation use each field's default from the chainspec BaseFeeConfig.
        let defaults = chain_spec.base_fee_config(1).resolve_calc_params(None);
        let expected_smoothed = GAS_USED * defaults.alpha / 100u64;
        assert_eq!(stored.gasUsedSmoothed, expected_smoothed);
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
        assert_ne!(
            stored.nextBaseFee, 0,
            "ADR-004 fallback must produce a non-zero base fee"
        );
    }

    /// Packs `(alpha, k_rate, inverse_elasticity_multiplier)` into the single storage word that
    /// ProtocolConfig stores at the ERC-7201 base slot.
    ///
    /// Layout (from `scripts/genesis/ProtocolConfig.ts`):
    ///   bits [0,63]    – alpha
    ///   bits [64,127]  – kRate
    ///   bits [128,191] – inverseElasticityMultiplier
    fn pack_fee_params_slot(
        alpha: u64,
        k_rate: u64,
        inverse_elasticity_multiplier: u64,
    ) -> StorageValue {
        U256::from(alpha)
            | (U256::from(k_rate) << 64)
            | (U256::from(inverse_elasticity_multiplier) << 128)
    }

    /// ERC-7201 base slot for ProtocolConfig storage.
    const PROTOCOL_CONFIG_FEE_PARAMS_SLOT: &str =
        "668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200";

    /// Overwrites the packed fee-params slot in ProtocolConfig storage with the given values,
    /// leaving minBaseFee/maxBaseFee/blockGasLimit untouched.
    fn patch_fee_params(
        db: &mut InMemoryDB,
        alpha: u64,
        k_rate: u64,
        inverse_elasticity_multiplier: u64,
    ) {
        let slot =
            StorageKey::from_str_radix(PROTOCOL_CONFIG_FEE_PARAMS_SLOT, 16).expect("valid hex");
        db.insert_account_storage(
            protocol_config::PROTOCOL_CONFIG_ADDRESS,
            slot,
            pack_fee_params_slot(alpha, k_rate, inverse_elasticity_multiplier),
        )
        .expect("insert storage");
    }

    #[test]
    fn test_zero4_invalid_alpha_stores_zero_next_base_fee() {
        let block_env = get_mock_block_env();
        let chain_spec = localdev_with_hardforks(&[(ArcHardfork::Zero3, 0)]);

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_fee_params(
            &mut db,
            255,
            GENESIS_K_RATE,
            GENESIS_INVERSE_ELASTICITY_MULTIPLIER,
        );

        let stored = run_executor_finish_and_query_gas_values(chain_spec, &block_env, &mut db);

        assert_eq!(stored.gasUsed, GAS_USED);
        assert_eq!(stored.gasUsedSmoothed, GAS_USED);
        assert_eq!(stored.nextBaseFee, 0);
    }

    #[test]
    fn test_zero5_executor_out_of_range_alpha_uses_default() {
        // alpha=255 exceeds alpha.max for localdev; zero5 will substitute alpha.default
        let block_env = get_mock_block_env();
        let chain_spec = LOCAL_DEV.clone();

        let defaults = chain_spec.base_fee_config(1).resolve_calc_params(None);

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_fee_params(
            &mut db,
            255,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );

        let stored = run_executor_finish_and_query_gas_values(chain_spec, &block_env, &mut db);

        // alpha=255 is out of range; alpha.default is used
        let expected_smoothed = GAS_USED * defaults.alpha / 100u64;
        assert_eq!(stored.gasUsedSmoothed, expected_smoothed);
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
    }

    #[test]
    fn test_zero5_executor_out_of_range_k_rate_uses_default() {
        let block_env = get_mock_block_env();
        let chain_spec = LOCAL_DEV.clone();

        let defaults = chain_spec.base_fee_config(1).resolve_calc_params(None);

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_fee_params(
            &mut db,
            defaults.alpha,
            20000,
            defaults.inverse_elasticity_multiplier,
        );

        let stored = run_executor_finish_and_query_gas_values(chain_spec, &block_env, &mut db);

        let expected_smoothed = GAS_USED * defaults.alpha / 100u64;
        // k_rate=20000 is out of range; k_rate.default must be used.
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
    }

    #[test]
    fn test_zero5_executor_out_of_range_elasticity_multiplier_uses_default() {
        let block_env = get_mock_block_env();
        let chain_spec = LOCAL_DEV.clone();

        let defaults = chain_spec.base_fee_config(1).resolve_calc_params(None);

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_fee_params(&mut db, defaults.alpha, defaults.k_rate, 0);

        let stored = run_executor_finish_and_query_gas_values(chain_spec, &block_env, &mut db);

        let expected_smoothed = GAS_USED * defaults.alpha / 100u64;
        // inverse_elasticity_multiplier=0 is below min; inverse_elasticity_multiplier.default must be used.
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            defaults.k_rate,
            defaults.inverse_elasticity_multiplier,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
    }

    #[test]
    fn test_zero5_executor_in_range_params_pass_through() {
        // All params are within bounds; the on-chain values must be used as-is (no substitution).
        let block_env = get_mock_block_env();
        let chain_spec = LOCAL_DEV.clone();

        // alpha=50 (in [0,100]), k_rate=500 (in [0,10000]), inverse_elasticity_multiplier=3000 (in [1,10000])
        const CUSTOM_ALPHA: u64 = 50;
        const CUSTOM_K_RATE: u64 = 500;
        const CUSTOM_ELASTICITY: u64 = 3000;

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_fee_params(&mut db, CUSTOM_ALPHA, CUSTOM_K_RATE, CUSTOM_ELASTICITY);

        let stored = run_executor_finish_and_query_gas_values(chain_spec, &block_env, &mut db);

        let expected_smoothed = GAS_USED * CUSTOM_ALPHA / 100u64;
        assert_eq!(stored.gasUsedSmoothed, expected_smoothed);
        let expected_next_base_fee = arc_calc_next_block_base_fee(
            expected_smoothed,
            block_env.gas_limit,
            block_env.basefee,
            CUSTOM_K_RATE,
            CUSTOM_ELASTICITY,
        );
        assert_eq!(stored.nextBaseFee, expected_next_base_fee);
    }

    #[test]
    fn test_zero5_executor_payload_rejects_mismatched_extra_data_base_fee() {
        let block_env = get_mock_block_env();
        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv {
            cfg_env,
            block_env: block_env.clone(),
        };

        let evm_config =
            crate::evm::ArcEvmConfig::new(reth_ethereum::evm::EthEvmConfig::new_with_evm_factory(
                chain_spec.clone(),
                crate::evm::ArcEvmFactory::new(chain_spec.clone()),
            ));

        let mut state = reth_ethereum::evm::revm::db::State::builder()
            .with_database(&mut db)
            .build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);

        let mut ctx = get_mock_execution_ctx();
        // Non-empty extra_data signals payload execution (consensus set the value); wrong on purpose
        ctx.extra_data = arc_execution_config::gas_fee::encode_base_fee_to_bytes(1);

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec,
            evm_config.inner.executor_factory.receipt_builder(),
        );
        executor.gas_used = GAS_USED;

        let err = executor
            .finish()
            .expect_err("Zero5 payload with mismatched extra_data must be rejected");
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("extra_data base fee mismatch"),
            "unexpected error: {err_msg}"
        );
    }

    #[test]
    fn test_beneficiary_validation_succeeds_when_matching() {
        // Test that beneficiary validation passes when header beneficiary matches ProtocolConfig
        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());

        let block_number = 10;

        // Query the expected beneficiary from ProtocolConfig
        let expected_beneficiary =
            query_expected_beneficiary(chain_spec.clone(), &mut db, block_number);

        // Create executor with correct beneficiary
        let evm_config = create_evm_config(chain_spec.clone());

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(block_number);
        block_env.beneficiary = expected_beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);

        let ctx = get_mock_execution_ctx();

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        // This should succeed because beneficiary matches
        let result = executor.apply_pre_execution_changes();
        assert!(
            result.is_ok(),
            "Beneficiary validation should succeed when beneficiary matches"
        );
    }

    #[test]
    fn test_beneficiary_validation_fails_when_mismatched() {
        // Test that beneficiary validation fails when header beneficiary doesn't match ProtocolConfig
        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());

        let evm_config = create_evm_config(chain_spec.clone());

        // Use a wrong beneficiary address
        let wrong_beneficiary = address!("0000000000000000000000000000000000000bad");

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(10);
        block_env.beneficiary = wrong_beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);

        let ctx = get_mock_execution_ctx();

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        // This should fail because beneficiary doesn't match
        let result = executor.apply_pre_execution_changes();
        assert!(
            result.is_err(),
            "Beneficiary validation should fail when beneficiary doesn't match"
        );

        // Verify the error is returned as a validation error.
        let err = result.unwrap_err();
        match err {
            BlockExecutionError::Validation(validation_err) => {
                let err_msg = validation_err.to_string();
                assert!(
                    err_msg.contains(ERR_BENEFICIARY_MISMATCH),
                    "Expected validation error containing '{}', got: {}",
                    ERR_BENEFICIARY_MISMATCH,
                    err_msg
                );
            }
            other => panic!(
                "Expected BlockExecutionError::Validation containing '{}', got {:?}",
                ERR_BENEFICIARY_MISMATCH, other
            ),
        }
    }

    #[test]
    fn test_validate_expected_beneficiary_allows_zero_expected() {
        let header_beneficiary = address!("0000000000000000000000000000000000000bad");
        let expected_beneficiary = Address::ZERO;

        let result = validate_expected_beneficiary(header_beneficiary, expected_beneficiary, 10);
        assert!(
            result.is_ok(),
            "Expected zero beneficiary should allow proposer-selected header beneficiary"
        );
    }

    #[test]
    fn test_validate_expected_beneficiary_rejects_mismatch() {
        let header_beneficiary = address!("0000000000000000000000000000000000000bad");
        let expected_beneficiary = address!("0000000000000000000000000000000000000bee");

        let err = validate_expected_beneficiary(header_beneficiary, expected_beneficiary, 10)
            .expect_err("Mismatched beneficiary should fail");
        match err {
            BlockExecutionError::Validation(validation_err) => {
                let err_msg = validation_err.to_string();
                assert!(
                    err_msg.contains(ERR_BENEFICIARY_MISMATCH),
                    "Expected validation error containing '{}', got: {}",
                    ERR_BENEFICIARY_MISMATCH,
                    err_msg
                );
            }
            other => panic!(
                "Expected BlockExecutionError::Validation containing '{}', got {:?}",
                ERR_BENEFICIARY_MISMATCH, other
            ),
        }
    }

    #[test]
    fn test_validate_beneficiary_not_blocklisted_rejects_blocklisted_address() {
        let mut db = InMemoryDB::default();
        let blocklisted_beneficiary = address!("0000000000000000000000000000000000000bad");
        mark_address_as_blocklisted(&mut db, blocklisted_beneficiary);

        let err = validate_beneficiary_not_blocklisted(&mut db, blocklisted_beneficiary, 10)
            .expect_err("Blocklisted beneficiary should be rejected");
        match err {
            BlockExecutionError::Validation(validation_err) => {
                let err_msg = validation_err.to_string();
                assert!(
                    err_msg.contains(ERR_BLOCKED_ADDRESS),
                    "Expected validation error containing '{}', got: {}",
                    ERR_BLOCKED_ADDRESS,
                    err_msg
                );
            }
            other => panic!("Expected BlockExecutionError::Validation, got {:?}", other),
        }
    }

    #[test]
    fn test_beneficiary_validation_skipped_before_zero5() {
        // Test that beneficiary validation is skipped for blocks before Zero5 hardfork
        let chain_spec = localdev_with_hardforks(&[(ArcHardfork::Zero4, 0)]);

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());

        let evm_config = create_evm_config(chain_spec.clone());

        // Use a wrong beneficiary - should still pass because we're before Zero5
        let wrong_beneficiary = address!("0000000000000000000000000000000000000bad");

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(0); // Before Zero5
        block_env.beneficiary = wrong_beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);

        let ctx = get_mock_execution_ctx();

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        // This should succeed because validation is skipped before Zero5
        let result = executor.apply_pre_execution_changes();
        assert!(
            result.is_ok(),
            "Beneficiary validation should be skipped before Zero5 hardfork"
        );
    }

    #[test]
    fn test_beneficiary_validation_continues_when_protocol_config_unavailable() {
        // Break ProtocolConfig so rewardBeneficiary() cannot be read
        // Should log error and continue (defensive: avoid halting chain)
        fn patch_protocol_config_to_invalid_impl(db: &mut InMemoryDB) {
            use reth_ethereum::evm::revm::primitives::{StorageKey, StorageValue};

            db.replace_account_storage(
                protocol_config::PROTOCOL_CONFIG_ADDRESS,
                HashMap::from_iter([(
                    StorageKey::from_str_radix(
                        "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
                        16,
                    )
                    .unwrap(),
                    StorageValue::from(0u64),
                )]),
            )
            .expect("Replace storage");
        }

        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        patch_protocol_config_to_invalid_impl(&mut db);

        let evm_config = create_evm_config(chain_spec.clone());

        // Any beneficiary; should continue despite ProtocolConfig call failure
        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(10);
        block_env.beneficiary = address!("0000000000000000000000000000000000000bad");

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);
        let ctx = get_mock_execution_ctx();
        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        let result = executor.apply_pre_execution_changes();
        assert!(
            result.is_ok(),
            "Should continue when ProtocolConfig call fails (defensive: avoid chain halt)"
        );
    }

    #[test]
    fn test_beneficiary_validation_skipped_when_zero_address() {
        // Test that beneficiary validation is skipped when ProtocolConfig returns 0x00
        // This allows for future proposer-set addresses
        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        set_beneficiary_to_zero_address(&mut db);

        let evm_config = create_evm_config(chain_spec.clone());

        // Use any beneficiary - should pass because ProtocolConfig returns 0x00
        let any_beneficiary = address!("0000000000000000000000000000000000000bad");

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(10);
        block_env.beneficiary = any_beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);

        let ctx = get_mock_execution_ctx();

        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        // This should succeed because validation is skipped for 0x00
        let result = executor.apply_pre_execution_changes();
        assert!(
            result.is_ok(),
            "Beneficiary validation should be skipped when ProtocolConfig returns 0x00"
        );
    }

    #[test]
    fn test_beneficiary_validation_fails_when_proposer_beneficiary_is_blocklisted() {
        let chain_spec = LOCAL_DEV.clone();

        let mut db = InMemoryDB::default();
        insert_alloc_into_db(&mut db, chain_spec.genesis());
        set_beneficiary_to_zero_address(&mut db);

        let blocklisted_beneficiary = address!("0000000000000000000000000000000000000bad");
        mark_address_as_blocklisted(&mut db, blocklisted_beneficiary);
        let expected_beneficiary = query_expected_beneficiary(chain_spec.clone(), &mut db, 10);
        assert!(
            expected_beneficiary.is_zero(),
            "ProtocolConfig.rewardBeneficiary() should be patched to zero for proposer-selected path"
        );
        let storage_slot = compute_is_blocklisted_storage_slot(blocklisted_beneficiary).into();
        let blocklist_status = <InMemoryDB as revm::Database>::storage(
            &mut db,
            arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS,
            storage_slot,
        )
        .expect("Read blocklist storage");
        assert_eq!(
            blocklist_status,
            StorageValue::from(1u64),
            "Beneficiary should be blocklisted in NativeCoinControl storage"
        );

        let evm_config = create_evm_config(chain_spec.clone());

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(10);
        block_env.beneficiary = blocklisted_beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);
        let ctx = get_mock_execution_ctx();
        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        let result = executor.apply_pre_execution_changes();
        match result {
            Err(BlockExecutionError::Validation(err)) => {
                let err_msg = err.to_string();
                assert!(
                    err_msg.contains(ERR_BLOCKED_ADDRESS),
                    "Expected validation error containing '{}', got: {}",
                    ERR_BLOCKED_ADDRESS,
                    err_msg
                );
            }
            other => panic!(
                "Expected BlockExecutionError::Validation containing '{}', got: {:?}",
                ERR_BLOCKED_ADDRESS, other
            ),
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("forced blocklist storage read failure")]
    struct ForcedBlocklistReadError;
    impl revm::database_interface::DBErrorMarker for ForcedBlocklistReadError {}

    #[derive(Debug)]
    struct BlocklistReadFailingDb {
        inner: InMemoryDB,
    }

    impl BlocklistReadFailingDb {
        fn new(inner: InMemoryDB) -> Self {
            Self { inner }
        }
    }

    impl revm::Database for BlocklistReadFailingDb {
        type Error = ForcedBlocklistReadError;

        fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            <InMemoryDB as revm::Database>::basic(&mut self.inner, address)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn code_by_hash(&mut self, code_hash: AlloyB256) -> Result<Bytecode, Self::Error> {
            <InMemoryDB as revm::Database>::code_by_hash(&mut self.inner, code_hash)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn storage(
            &mut self,
            address: Address,
            index: StorageKey,
        ) -> Result<StorageValue, Self::Error> {
            if address == arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS {
                return Err(ForcedBlocklistReadError);
            }
            <InMemoryDB as revm::Database>::storage(&mut self.inner, address, index)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn block_hash(&mut self, number: u64) -> Result<AlloyB256, Self::Error> {
            <InMemoryDB as revm::Database>::block_hash(&mut self.inner, number)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }
    }

    #[test]
    fn test_beneficiary_validation_fails_when_blocklist_read_fails() {
        let chain_spec = LOCAL_DEV.clone();

        let mut base_db = InMemoryDB::default();
        insert_alloc_into_db(&mut base_db, chain_spec.genesis());
        set_beneficiary_to_zero_address(&mut base_db);

        let db = BlocklistReadFailingDb::new(base_db);
        let evm_config = create_evm_config(chain_spec.clone());
        let beneficiary = address!("0000000000000000000000000000000000000bad");

        let mut block_env = get_mock_block_env();
        block_env.number = U256::from(10);
        block_env.beneficiary = beneficiary;

        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        let evm_env = EvmEnv { cfg_env, block_env };

        let mut state = State::builder().with_database(db).build();
        let evm = evm_config.evm_with_env(&mut state, evm_env);
        let ctx = get_mock_execution_ctx();
        let mut executor = ArcBlockExecutor::new(
            evm,
            ctx,
            chain_spec.as_ref(),
            evm_config.inner.executor_factory.receipt_builder(),
        );

        let result = executor.apply_pre_execution_changes();
        match result {
            Err(BlockExecutionError::Validation(validation_err)) => {
                let err_msg = validation_err.to_string();
                assert!(
                    err_msg.contains(ERR_BLOCKLIST_READ_FAILED),
                    "Expected validation error containing '{}', got: {}",
                    ERR_BLOCKLIST_READ_FAILED,
                    err_msg
                );
            }
            other => panic!(
                "Expected BlockExecutionError::Validation containing '{}', got: {:?}",
                ERR_BLOCKLIST_READ_FAILED, other
            ),
        }
    }
}
