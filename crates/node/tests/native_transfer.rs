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

mod common;
use common::{setup_evm, setup_evm_with_inspector, WALLET_RECEIVER_INDEX, WALLET_SENDER_INDEX};

use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, Log};
use alloy_rlp::Bytes;
use alloy_rpc_types_trace::geth::call::CallConfig;
use alloy_sol_types::SolEvent;
use arc_evm::ArcEvm;
use arc_precompiles::NATIVE_COIN_AUTHORITY_ADDRESS;
use common::NativeCoinAuthority;
use reth_chainspec::{EthChainSpec, DEV};
use reth_e2e_test_utils::wallet::Wallet;
use reth_ethereum::evm::revm::{inspector::Inspector, interpreter::interpreter::EthInterpreter};
use reth_evm::Evm;
use revm::handler::SYSTEM_ADDRESS;
use revm::{
    context::result::{ExecResultAndState, ExecutionResult},
    handler::{instructions::EthInstructions, PrecompileProvider},
    interpreter::InterpreterResult,
};
use revm::{context::TxEnv, database::InMemoryDB};
use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};
use revm_primitives::{hardfork::SpecId, TxKind, U256};

use arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS;

const EIP7708_LOG_ADDRESS: Address = SYSTEM_ADDRESS;

fn test_native_transfer<I, PRECOMPILE>(
    evm: &mut ArcEvm<
        EthEvmContext<InMemoryDB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
        PRECOMPILE,
    >,
    wallet: &Wallet,
) -> (TxEnv, ExecResultAndState<ExecutionResult>)
where
    I: Inspector<EthEvmContext<InMemoryDB>, EthInterpreter>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<InMemoryDB>, Output = InterpreterResult>,
{
    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();
    let amount = U256::from(42);
    // Zero6 hardfork requires extra gas for blocklist SLOAD checks:
    // - Base intrinsic gas: 21000
    // - Caller blocklist check: 2100
    // - Recipient blocklist check (value > 0): 2100
    // Total: 25200
    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000, // Must exceed 25200 for Zero6 blocklist gas
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx.clone()).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}",);
    // Localdev activates Zero5: CALL transfers emit an EIP-7708 Transfer log
    // from the system address 0xfffe...fffe instead of NativeCoinTransferred.
    let logs = exec.result.logs();
    assert_eq!(logs.len(), 1);
    let log: &Log = &logs[0];
    assert_eq!(log.address, EIP7708_LOG_ADDRESS);
    let decoded = common::NativeFiatTokenV2_2::Transfer::decode_log(log)
        .expect("Failed to decode EIP-7708 Transfer log");
    assert_eq!(decoded.from, sender.address());
    assert_eq!(decoded.to, receiver.address());
    assert_eq!(decoded.value, amount);

    let balance_before = evm
        .db_mut()
        .load_account(sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);

    let balance_after = exec
        .state
        .get(&sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(balance_after + amount, balance_before);

    (tx, exec)
}

#[test]
fn evm_native_transfer() {
    let (mut evm, wallet) = setup_evm();
    test_native_transfer(&mut evm, &wallet);
}

#[test]
fn inspect_evm_native_transfer() {
    let call_config = CallConfig {
        with_log: Some(true),
        only_top_call: Some(false),
    };
    let mut inspector =
        TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&call_config));
    let (mut evm, wallet) = setup_evm_with_inspector(&mut inspector);

    let (tx, exec) = test_native_transfer(&mut evm, &wallet);

    let frame = inspector
        .with_transaction_gas_limit(26_000)
        .into_geth_builder()
        .geth_call_traces(call_config, exec.result.gas_used());

    assert_eq!(frame.from, tx.caller);
    assert_eq!(frame.to, tx.kind.to().cloned());
    assert_eq!(frame.value, Some(tx.value));
    assert_eq!(frame.input, Bytes::new());
    assert_eq!(frame.output, None);
    assert_eq!(frame.gas, 26_000);
    assert_eq!(frame.error, None);
    assert_eq!(frame.revert_reason, None);
    assert_eq!(frame.calls.len(), 0);

    assert_eq!(frame.logs.len(), 0);

    assert_eq!(frame.typ, "CALL");
}

/// Helper to create a chainspec with Zero5 active at block 0.
fn chainspec_with_zero5() -> std::sync::Arc<arc_execution_config::chainspec::ArcChainSpec> {
    use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
    localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 0),
    ])
}

/// Zero5: plain CALL value transfer emits 1 EIP-7708 Transfer log.
#[test]
fn evm_native_transfer_zero5_eip7708_log() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();
    let amount = U256::from(42);

    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    let logs = exec.result.logs();
    assert_eq!(logs.len(), 1, "Zero5 should emit 1 EIP-7708 Transfer log");

    assert_eq!(
        logs[0].address, EIP7708_LOG_ADDRESS,
        "Log should be from EIP-7708 system address"
    );

    // Decode as ERC-20 Transfer event — same topic signature as NativeCoinTransferred
    // but emitted from the EIP-7708 system address instead of the native coin authority.
    let decoded = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs[0])
        .expect("Failed to decode EIP-7708 Transfer log");
    assert_eq!(decoded.from, sender.address());
    assert_eq!(decoded.to, receiver.address());
    assert_eq!(decoded.value, amount);
}

/// Pre-Zero5: plain CALL value transfer emits 1 NativeCoinTransferred log.
#[test]
fn evm_native_transfer_pre_zero5_emits_native_coin_transferred() {
    use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
    // Zero4 active but NOT Zero5
    let chain_spec = localdev_with_hardforks(&[(ArcHardfork::Zero3, 0), (ArcHardfork::Zero4, 0)]);
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();
    let amount = U256::from(42);

    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    // Pre-Zero5: should emit 1 NativeCoinTransferred log
    let logs = exec.result.logs();
    assert_eq!(
        logs.len(),
        1,
        "Pre-Zero5 should emit 1 NativeCoinTransferred log"
    );
    let log: &Log = &logs[0];
    assert_eq!(log.address, NATIVE_COIN_AUTHORITY_ADDRESS);
    let decoded = NativeCoinAuthority::NativeCoinTransferred::decode_log(log)
        .expect("Failed to decode NativeCoinTransferred log");
    assert_eq!(decoded.from, sender.address());
    assert_eq!(decoded.to, receiver.address());
    assert_eq!(decoded.amount, amount);
}

/// Helper to create a chainspec with Zero4 active but NOT Zero5.
fn chainspec_pre_zero5() -> std::sync::Arc<arc_execution_config::chainspec::ArcChainSpec> {
    use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
    localdev_with_hardforks(&[(ArcHardfork::Zero3, 0), (ArcHardfork::Zero4, 0)])
}

/// B1: Zero5 self-transfer (sender == receiver) with non-zero value produces no logs.
/// EIP-7708 specifies Transfer logs only for "nonzero-value-transferring ... to a different account".
#[test]
fn evm_native_transfer_zero5_self_transfer_no_log() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let amount = U256::from(42);

    // Self-transfer: sender == receiver
    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(sender.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    // EIP-7708: self-transfers do not emit a Transfer log
    let logs = exec.result.logs();
    assert_eq!(logs.len(), 0, "Zero5: self-transfer should emit 0 logs");

    // Balance should be unchanged (self-transfer)
    let balance_before = evm
        .db_mut()
        .load_account(sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    let balance_after = exec
        .state
        .get(&sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        balance_after, balance_before,
        "balance should be unchanged for self-transfer"
    );
}

/// B2: Zero5 zero-value CALL produces no logs.
/// EIP-7708 specifies Transfer logs only for "nonzero-value-transferring" operations.
#[test]
fn evm_native_transfer_zero5_zero_value_no_log() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();

    // Zero value transfer
    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: U256::ZERO,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    // Zero-value transfers do not emit any log
    let logs = exec.result.logs();
    assert_eq!(logs.len(), 0, "Zero5: zero-value CALL should emit 0 logs");
}

/// B3: Zero5 CREATE with value emits an EIP-7708 Transfer log.
/// The log should be from the EIP-7708 system address (0xfffe...fffe) with from=caller, to=created.
#[test]
fn evm_native_transfer_zero5_create_with_value_emits_eip7708_log() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let amount = U256::from(42);

    // Minimal contract bytecode: PUSH1 0x00 PUSH1 0x00 RETURN (deploys empty contract)
    let creation_code: alloy_primitives::Bytes = vec![0x60, 0x00, 0x60, 0x00, 0xF3].into();

    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Create,
        value: amount,
        gas_limit: 100_000,
        gas_price: 0,
        data: creation_code,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    let logs = exec.result.logs();
    assert_eq!(
        logs.len(),
        1,
        "Zero5: CREATE with value should emit 1 EIP-7708 Transfer log"
    );

    assert_eq!(logs[0].address, EIP7708_LOG_ADDRESS);

    let decoded = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs[0])
        .expect("Failed to decode EIP-7708 Transfer log");
    assert_eq!(decoded.from, sender.address());
    // `to` should be the newly created contract address
    assert_ne!(
        decoded.to,
        alloy_primitives::Address::ZERO,
        "created address should not be zero"
    );
    assert_eq!(decoded.value, amount);
}

/// B4: Zero5 CALL to Address::ZERO with non-zero value should revert.
/// Arc disallows value transfers to the zero address at the EVM level under Zero5.
#[test]
fn evm_native_transfer_zero5_to_zero_address_reverts() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) = common::setup_evm_with_chainspec(chain_spec);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let amount = U256::from(42);

    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(alloy_primitives::Address::ZERO),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");

    // Should revert due to zero address check
    assert!(
        !exec.result.is_success(),
        "Zero5: CALL to Address::ZERO with value should revert"
    );

    // Balance should be unchanged (revert)
    let balance_before = evm
        .db_mut()
        .load_account(sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    let balance_after = exec
        .state
        .get(&sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        balance_after, balance_before,
        "balance should be unchanged after revert"
    );
}

/// B6: Event ordering is preserved across the Zero5 hardfork boundary.
/// The Transfer log should appear at the same position (index 0) as the old
/// NativeCoinTransferred log for plain CALL value transfers.
#[test]
fn evm_native_transfer_event_ordering_preserved_across_zero5() {
    let sender_idx = WALLET_SENDER_INDEX;
    let receiver_idx = WALLET_RECEIVER_INDEX;
    let amount = U256::from(42);

    // Pre-Zero5: NativeCoinTransferred
    let chain_spec_pre = chainspec_pre_zero5();
    let (mut evm_pre, wallet) = common::setup_evm_with_chainspec(chain_spec_pre);
    let sender = wallet.wallet_gen()[sender_idx].clone();
    let receiver = wallet.wallet_gen()[receiver_idx].clone();
    let tx_pre = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };
    let exec_pre = evm_pre
        .transact_raw(tx_pre)
        .expect("Pre-Zero5 tx should be accepted");
    assert!(exec_pre.result.is_success());

    // Zero5: EIP-7708 Transfer
    let chain_spec_z5 = chainspec_with_zero5();
    let (mut evm_z5, wallet_z5) = common::setup_evm_with_chainspec(chain_spec_z5);
    let sender_z5 = wallet_z5.wallet_gen()[sender_idx].clone();
    let receiver_z5 = wallet_z5.wallet_gen()[receiver_idx].clone();
    let tx_z5 = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender_z5.address(),
        kind: TxKind::Call(receiver_z5.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };
    let exec_z5 = evm_z5
        .transact_raw(tx_z5)
        .expect("Zero5 tx should be accepted");
    assert!(exec_z5.result.is_success());

    let logs_pre = exec_pre.result.logs();
    let logs_z5 = exec_z5.result.logs();

    // Same number of logs
    assert_eq!(
        logs_pre.len(),
        logs_z5.len(),
        "log count should be identical"
    );
    assert_eq!(
        logs_pre.len(),
        1,
        "plain CALL value transfer should emit exactly 1 log"
    );

    // Both logs are at index 0 — ordering is preserved
    // Pre-Zero5: NativeCoinTransferred from authority address
    assert_eq!(logs_pre[0].address, NATIVE_COIN_AUTHORITY_ADDRESS);
    let decoded_pre = NativeCoinAuthority::NativeCoinTransferred::decode_log(&logs_pre[0])
        .expect("Failed to decode NativeCoinTransferred");
    assert_eq!(decoded_pre.from, sender.address());
    assert_eq!(decoded_pre.to, receiver.address());
    assert_eq!(decoded_pre.amount, amount);

    // Zero5: EIP-7708 Transfer from system address
    assert_eq!(logs_z5[0].address, EIP7708_LOG_ADDRESS);
    let decoded_z5 = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs_z5[0])
        .expect("Failed to decode EIP-7708 Transfer");
    assert_eq!(decoded_z5.from, sender_z5.address());
    assert_eq!(decoded_z5.to, receiver_z5.address());
    assert_eq!(decoded_z5.value, amount);
}

/// Zero5 + AMSTERDAM: Arc self-emits EIP-7708 Transfer log regardless of SpecId.
/// When REVM is eventually upgraded with native EIP-7708 journal support, we may see
/// duplicate logs (one from Arc, one from REVM). That migration is tracked separately.
#[test]
fn evm_native_transfer_zero5_amsterdam_eip7708_log() {
    let chain_spec = chainspec_with_zero5();
    let (mut evm, wallet) =
        common::setup_evm_with_chainspec_and_spec(chain_spec, SpecId::AMSTERDAM);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();
    let amount = U256::from(42);

    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec: ExecResultAndState<ExecutionResult> =
        evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");

    let logs = exec.result.logs();
    assert_eq!(
        logs.len(),
        1,
        "Zero5 should emit 1 EIP-7708 Transfer log (self-implemented)"
    );

    assert_eq!(logs[0].address, EIP7708_LOG_ADDRESS);

    // Verify balance was still transferred correctly
    let balance_before = evm
        .db_mut()
        .load_account(sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    let balance_after = exec
        .state
        .get(&sender.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(balance_after + amount, balance_before);
}

/// Hardfork boundary test: proves that behavior switches exactly at the Zero5 activation block.
/// Block 9 (pre-Zero5) emits NativeCoinTransferred from NATIVE_COIN_AUTHORITY_ADDRESS;
/// Block 10 (Zero5 active) emits EIP-7708 Transfer from the system address.
#[test]
fn evm_native_transfer_hardfork_boundary_zero5_activation() {
    use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
    use reth_evm::ConfigureEvm;

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 10),
    ]);

    let amount = U256::from(42);

    // --- Block 9: pre-Zero5 ---
    let (evm_config, db, mut evm_env, wallet) =
        common::setup_evm_env_with_chainspec(chain_spec.clone());
    evm_env.block_env.number = U256::from(9);
    let mut evm_pre = evm_config.evm_with_env(db, evm_env);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();

    let tx_pre = TxEnv {
        chain_id: Some(chain_spec.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec_pre = evm_pre
        .transact_raw(tx_pre)
        .expect("Pre-Zero5 tx should be accepted");
    assert!(
        exec_pre.result.is_success(),
        "block 9 tx should succeed: {:?}",
        exec_pre.result
    );

    let logs_pre = exec_pre.result.logs();
    assert_eq!(
        logs_pre.len(),
        1,
        "block 9: should emit exactly 1 NativeCoinTransferred log"
    );
    assert_eq!(
        logs_pre[0].address, NATIVE_COIN_AUTHORITY_ADDRESS,
        "block 9: log should come from NativeCoinAuthority"
    );
    let decoded_pre = NativeCoinAuthority::NativeCoinTransferred::decode_log(&logs_pre[0])
        .expect("Failed to decode NativeCoinTransferred log at block 9");
    assert_eq!(decoded_pre.from, sender.address());
    assert_eq!(decoded_pre.to, receiver.address());
    assert_eq!(decoded_pre.amount, amount);

    // --- Block 10: Zero5 active ---
    let (evm_config, db, mut evm_env, wallet) =
        common::setup_evm_env_with_chainspec(chain_spec.clone());
    evm_env.block_env.number = U256::from(10);
    let mut evm_z5 = evm_config.evm_with_env(db, evm_env);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let receiver = wallet.wallet_gen()[WALLET_RECEIVER_INDEX].clone();

    let tx_z5 = TxEnv {
        chain_id: Some(chain_spec.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(receiver.address()),
        value: amount,
        gas_limit: 26_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec_z5 = evm_z5
        .transact_raw(tx_z5)
        .expect("Zero5 tx should be accepted");
    assert!(
        exec_z5.result.is_success(),
        "block 10 tx should succeed: {:?}",
        exec_z5.result
    );

    let logs_z5 = exec_z5.result.logs();
    assert_eq!(
        logs_z5.len(),
        1,
        "block 10: should emit exactly 1 EIP-7708 Transfer log"
    );
    assert_eq!(
        logs_z5[0].address, EIP7708_LOG_ADDRESS,
        "block 10: log should come from EIP-7708 system address"
    );
    let decoded_z5 = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs_z5[0])
        .expect("Failed to decode EIP-7708 Transfer log at block 10");
    assert_eq!(decoded_z5.from, sender.address());
    assert_eq!(decoded_z5.to, receiver.address());
    assert_eq!(decoded_z5.value, amount);
}

/// Blocklist enforcement must be identical on inspect and non-inspect paths.
///
/// If `ArcEvm::inspect_frame_init` delegates to the inner EVM instead of going through
/// `ArcEvm::frame_init`, the blocklist check in `before_frame_init` is bypassed. The
/// top-level tx validation catches blocklisted addresses, but for *nested* calls the only
/// guard is `before_frame_init` (called from `frame_init`).
///
/// This test deploys a contract that performs a nested CALL with value to a blocklisted
/// address and verifies both the non-inspect and inspect paths produce the same revert.
#[test]
fn blocklist_enforced_on_inspect_path() {
    use arc_execution_config::native_coin_control::compute_is_blocklisted_storage_slot;
    use reth_evm::ConfigureEvm;
    use revm_primitives::keccak256;

    use revm::bytecode::opcode;

    let blocklisted_target = Address::repeat_byte(0xBB);

    // Runtime bytecode: CALL(gas, target, 1 wei, 0, 0, 0, 0); POP; STOP
    #[rustfmt::skip]
    let mut code: Vec<u8> = vec![
        opcode::PUSH1, 0x00, // retLength
        opcode::PUSH1, 0x00, // retOffset
        opcode::PUSH1, 0x00, // argsLength
        opcode::PUSH1, 0x00, // argsOffset
        opcode::PUSH1, 0x01, // value = 1 wei
        opcode::PUSH20,      // target address (20 bytes follow)
    ];
    code.extend_from_slice(blocklisted_target.as_ref());
    #[rustfmt::skip]
    code.extend_from_slice(&[
        opcode::GAS,  // forward all remaining gas
        opcode::CALL, // CALL(gas, target, value, argsOff, argsSz, retOff, retSz)
        opcode::POP,  // discard success flag
        opcode::STOP,
    ]);

    let factory_addr = Address::repeat_byte(0xFA);

    // --- Helper: set up DB with blocklisted target and factory contract ---
    let setup_db = |db: &mut InMemoryDB| {
        // Blocklist the target
        let slot = compute_is_blocklisted_storage_slot(blocklisted_target);
        db.insert_account_storage(NATIVE_COIN_CONTROL_ADDRESS, slot.into(), U256::from(1))
            .expect("insert blocklist storage");

        // Insert factory contract with some balance
        let bytecode = code.clone();
        db.insert_account_info(
            factory_addr,
            revm::state::AccountInfo {
                balance: U256::from(10_000),
                nonce: 1,
                code_hash: keccak256(&bytecode),
                code: Some(revm::state::Bytecode::new_raw(bytecode.into())),
                account_id: None,
            },
        );
    };

    // --- Non-inspect path ---
    let (evm_config, mut db, evm_env, wallet) = common::setup_evm_env();
    setup_db(&mut db);
    let mut evm = evm_config.evm_with_env(db, evm_env);

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let tx = TxEnv {
        chain_id: Some(reth_chainspec::DEV.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(factory_addr),
        value: U256::ZERO,
        gas_limit: 100_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec_no_inspect = evm.transact_raw(tx.clone()).expect("tx should be accepted");
    // The top-level tx succeeds but the nested CALL to the blocklisted address reverts.
    // The factory's CALL returns 0 (failure) on the stack, but since we STOP right after,
    // the overall tx succeeds. The key check: the blocklisted target should NOT receive funds.
    assert!(
        exec_no_inspect.result.is_success(),
        "non-inspect: top-level tx should succeed (nested call reverts internally)"
    );
    let target_balance_no_inspect = exec_no_inspect
        .state
        .get(&blocklisted_target)
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        target_balance_no_inspect,
        U256::ZERO,
        "non-inspect: blocklisted target should not receive funds via nested CALL"
    );

    // --- Inspect path ---
    let (evm_config, mut db, evm_env, _wallet) = common::setup_evm_env();
    setup_db(&mut db);
    let call_config = CallConfig {
        with_log: Some(true),
        only_top_call: Some(false),
    };
    let inspector =
        TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&call_config));
    let mut evm_inspect = evm_config.evm_with_env_and_inspector(db, evm_env, inspector);

    let exec_inspect = evm_inspect.transact_raw(tx).expect("tx should be accepted");
    assert!(
        exec_inspect.result.is_success(),
        "inspect: top-level tx should succeed (nested call reverts internally)"
    );
    let target_balance_inspect = exec_inspect
        .state
        .get(&blocklisted_target)
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        target_balance_inspect,
        U256::ZERO,
        "inspect: blocklisted target should not receive funds via nested CALL"
    );

    // Both paths should consume the same gas
    assert_eq!(
        exec_no_inspect.result.gas_used(),
        exec_inspect.result.gas_used(),
        "inspect and non-inspect paths should consume identical gas"
    );
}

/// CREATE2 with value under Zero5 emits an EIP-7708 Transfer log with the correct
/// deterministic CREATE2 address as `to`.
#[test]
fn evm_native_create2_with_value_zero5_emits_eip7708_log() {
    use reth_evm::ConfigureEvm;
    use revm_primitives::keccak256;

    let chain_spec = chainspec_with_zero5();
    let (evm_config, mut db, evm_env, wallet) =
        common::setup_evm_env_with_chainspec(chain_spec.clone());

    let sender = wallet.wallet_gen()[WALLET_SENDER_INDEX].clone();
    let factory = Address::repeat_byte(0xFA);
    let endowment = U256::from(42);
    let salt = U256::from(0xBEEF_u64);

    // Child init code: PUSH1 0x00 PUSH1 0x00 RETURN (deploys empty contract)
    let child_initcode: Vec<u8> = vec![0x60, 0x00, 0x60, 0x00, 0xF3];
    let child_initcode_len = child_initcode.len();
    let child_initcode_hash = keccak256(&child_initcode);

    // Derive the expected CREATE2 address:
    // address = keccak256(0xff ++ factory ++ salt ++ keccak256(initcode))[12..]
    let expected_create2_addr = factory.create2(salt.to_be_bytes(), child_initcode_hash);

    // Factory runtime bytecode that:
    //   1. Copies child initcode from code into memory
    //   2. Executes CREATE2(value=CALLVALUE, offset=0, size=initcode_len, salt)
    //   3. STOPs
    //
    // Layout (child initcode is appended after the opcodes):
    //   PUSH1 <initcode_len>    ; size
    //   PUSH1 <initcode_offset> ; offset in code (after these opcodes)
    //   PUSH1 0x00              ; destOffset in memory
    //   CODECOPY                ; mem[0..len] = code[offset..offset+len]
    //   PUSH32 <salt>           ; salt
    //   PUSH1 <initcode_len>    ; size
    //   PUSH1 0x00              ; offset in memory
    //   CALLVALUE               ; endowment from msg.value
    //   CREATE2                 ; CREATE2(value, offset, size, salt)
    //   STOP
    // Factory runtime bytecode prefix (initcode offset at byte 3 is patched below).
    //   PUSH1 <len>  PUSH1 <offset>  PUSH1 0x00  CODECOPY
    //   PUSH32 <salt>  PUSH1 <len>  PUSH1 0x00  CALLVALUE  CREATE2  STOP
    let mut prefix: Vec<u8> = vec![
        0x60,
        child_initcode_len as u8, // PUSH1 <initcode_len>
        0x60,
        0x00, // PUSH1 <initcode_offset> — patched below
        0x60,
        0x00, // PUSH1 0x00 (destOffset)
        0x39, // CODECOPY
        0x7F, // PUSH32 (salt follows)
    ];
    prefix.extend_from_slice(&salt.to_be_bytes::<32>());
    prefix.extend_from_slice(&[
        0x60,
        child_initcode_len as u8, // PUSH1 <initcode_len>
        0x60,
        0x00, // PUSH1 0x00 (offset)
        0x34, // CALLVALUE
        0xF5, // CREATE2
        0x00, // STOP
    ]);

    // Patch the initcode offset (byte index 3) — initcode is appended after prefix.
    prefix[3] = prefix.len() as u8;

    let mut factory_code = prefix;
    factory_code.extend_from_slice(&child_initcode);

    let factory_runtime = revm::state::Bytecode::new_raw(factory_code.clone().into());

    // Insert factory account with balance to fund the CREATE2
    db.insert_account_info(
        factory,
        revm::state::AccountInfo {
            balance: U256::from(10_000),
            nonce: 1,
            code_hash: keccak256(&factory_code),
            code: Some(factory_runtime),
            account_id: None,
        },
    );

    let mut evm = evm_config.evm_with_env(db, evm_env);

    let tx = TxEnv {
        chain_id: Some(chain_spec.chain_id()),
        caller: sender.address(),
        kind: TxKind::Call(factory),
        value: endowment,
        gas_limit: 200_000,
        gas_price: 0,
        ..Default::default()
    };

    let exec = evm.transact_raw(tx).expect("CREATE2 tx should be accepted");
    assert!(
        exec.result.is_success(),
        "CREATE2 with value should succeed: {:?}",
        exec.result
    );

    // Two EIP-7708 Transfer logs expected:
    // 1. sender → factory (top-level CALL value transfer)
    // 2. factory → create2_addr (CREATE2 endowment)
    let logs = exec.result.logs();
    assert_eq!(
        logs.len(),
        2,
        "Expected 2 EIP-7708 Transfer logs (CALL + CREATE2), got {}",
        logs.len()
    );

    // First log: sender → factory (the top-level value transfer)
    assert_eq!(logs[0].address, EIP7708_LOG_ADDRESS);
    let decoded_call = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs[0])
        .expect("Failed to decode CALL Transfer log");
    assert_eq!(decoded_call.from, sender.address());
    assert_eq!(decoded_call.to, factory);
    assert_eq!(decoded_call.value, endowment);

    // Second log: factory → create2_addr (the CREATE2 endowment)
    assert_eq!(logs[1].address, EIP7708_LOG_ADDRESS);
    let decoded_create2 = common::NativeFiatTokenV2_2::Transfer::decode_log(&logs[1])
        .expect("Failed to decode CREATE2 Transfer log");
    assert_eq!(
        decoded_create2.from, factory,
        "CREATE2 Transfer from should be the factory"
    );
    assert_eq!(
        decoded_create2.to, expected_create2_addr,
        "CREATE2 Transfer to should be the deterministic CREATE2 address"
    );
    assert_eq!(
        decoded_create2.value, endowment,
        "CREATE2 Transfer amount should match the endowment"
    );
}
