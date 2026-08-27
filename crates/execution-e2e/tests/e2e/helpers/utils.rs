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

use alloy_primitives::{Address, Bytes, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionReceipt, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use arc_execution_e2e::{ArcTestNode, TxKind};
use eyre::Result;

pub fn fee<T>(receipt: &TransactionReceipt<T>) -> U256 {
    U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price)
}

/// Sends a signed transaction, produces one block, and returns its receipt.
pub async fn send_and_mine(
    node: &mut ArcTestNode,
    signer: PrivateKeySigner,
    request: TransactionRequest,
) -> Result<TransactionReceipt> {
    let tx_hash = node.send_tx(signer, request).await?;
    node.produce_block().await?;
    node.get_receipt(tx_hash).await
}

/// Deploys `bytecode` via CREATE, mines it, and returns the created address and receipt.
///
/// Panics if the deploy transaction reverts or the receipt omits the contract address.
pub async fn deploy_and_mine(
    node: &mut ArcTestNode,
    signer: PrivateKeySigner,
    bytecode: Bytes,
    value: U256,
    gas: u64,
) -> Result<(Address, TransactionReceipt)> {
    let from = signer.address();
    let receipt = send_and_mine(
        node,
        signer,
        TransactionRequest {
            from: Some(from),
            to: Some(TxKind::Create),
            value: Some(value),
            gas: Some(gas),
            input: TransactionInput::new(bytecode),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    let address = receipt
        .contract_address
        .expect("successful CREATE receipt must include contract address");
    Ok((address, receipt))
}
