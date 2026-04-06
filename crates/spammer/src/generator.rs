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

use alloy_consensus::{SignableTransaction, Signed, TxEip1559};
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_signer::Signer;
use alloy_signer_local::LocalSigner;
use alloy_sol_types::{sol, SolCall};
use color_eyre::eyre::{self, Result};
use k256::ecdsa::SigningKey;
use rand::Rng;
use serde_json::json;
use std::collections::HashSet;
use std::ops::Range;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};

use crate::accounts::AccountBuilder;
use crate::config::{
    Erc20FnWeights, Erc20Function, GuzzlerFnWeights, GuzzlerFunction, TxType, TxTypeMix,
};
use crate::ws::{WsClient, WsClientBuilder};

use crate::erc20::TEST_TOKEN_ADDRESS;

pub(crate) const TESTNET_CHAIN_ID: u64 = 1337;
const GUZZLER_ADDRESS: Address = address!("1be052abb35D7f41609Bfec8F2fC2A684CB9984f");

/// Generates and signs transactions from a pool of pre-funded genesis accounts.
///
/// Each generator is assigned a non-overlapping slice of the account space and
/// cycles through its accounts in round-robin order. It supports three
/// transaction types, selected per-transaction according to configurable
/// weights ([`TxTypeMix`]):
///
/// - **Native transfers** -- simple value transfers between accounts.
/// - **ERC-20 calls** -- `transfer`, `approve`, and `transferFrom` against
///   a deployed `TestToken` contract, with function mix controlled by
///   [`Erc20FnWeights`].
/// - **GasGuzzler calls** -- gas-intensive operations (`hashLoop`,
///   `storageWrite`, `storageRead`, `guzzle`, `guzzle2`) against a deployed
///   `GasGuzzler` contract, with function mix controlled by
///   [`GuzzlerFnWeights`].
///
/// In fire-and-forget mode the generator pushes signed transactions into a
/// channel for a separate [`TxSender`](crate::sender::TxSender) task; in backpressure mode the sender
/// owns the generator directly and calls [`next_tx`](Self::next_tx).
pub(crate) struct TxGenerator {
    id: usize,
    signers: Vec<Option<LocalSigner<SigningKey>>>,
    signers_range: Range<usize>,
    next_nonces: Vec<Option<u64>>,
    account_builder: AccountBuilder,
    ws_client_builders: Vec<WsClientBuilder>,
    /// Channel to send signed txs to a separate `TxSender` task (fire-and-forget mode).
    /// `None` in backpressure mode, where the sender owns the generator directly.
    tx_sender: Option<Sender<Signed<TxEip1559>>>,
    max_txs_per_account: u64,
    query_latest_nonce: bool,
    tx_input_size: usize,
    guzzler_fn_weights: GuzzlerFnWeights,
    erc20_fn_weights: Erc20FnWeights,
    tx_type_mix: TxTypeMix,
    /// Lazily built WS clients (used by next_tx for guzzler gas estimation and nonce queries)
    ws_clients: Option<Vec<WsClient>>,
    /// Whether the GasGuzzler contract has been verified as deployed
    guzzler_verified: bool,
    /// Whether the TestToken contract has been verified as deployed
    test_token_verified: bool,
    /// Per-account transaction count (used by next_tx to enforce max_txs_per_account)
    tx_counts: Vec<u64>,
    /// Round-robin index for next_tx
    next_account_index: usize,
    /// When true, init() eagerly queries nonces for all accounts
    query_nonces_on_init: bool,
    /// Accounts permanently excluded from round-robin (e.g., after repeated failures)
    skipped_accounts: HashSet<usize>,
}

impl TxGenerator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: usize,
        signers_range: Range<usize>,
        account_builder: AccountBuilder,
        ws_client_builders: Vec<WsClientBuilder>,
        tx_sender: Option<Sender<Signed<TxEip1559>>>,
        max_txs_per_account: u64,
        query_latest_nonce: bool,
        tx_input_size: usize,
        guzzler_fn_weights: GuzzlerFnWeights,
        erc20_fn_weights: Erc20FnWeights,
        tx_type_mix: TxTypeMix,
    ) -> Self {
        let size = signers_range.len();
        Self {
            id,
            signers: vec![None; size],
            signers_range,
            next_nonces: vec![None; size],
            account_builder,
            ws_client_builders,
            tx_sender,
            max_txs_per_account,
            query_latest_nonce,
            tx_input_size,
            guzzler_fn_weights,
            erc20_fn_weights,
            tx_type_mix,
            ws_clients: None,
            guzzler_verified: false,
            test_token_verified: false,
            tx_counts: vec![0; size],
            next_account_index: 0,
            query_nonces_on_init: false,
            skipped_accounts: HashSet::new(),
        }
    }

    fn select_guzzler_function(&self) -> Result<GuzzlerFunction> {
        let total = self.guzzler_fn_weights.total_weight();
        if total == 0 {
            eyre::bail!("select_guzzler_function called with total weight 0");
        }
        let mut pick = rand::thread_rng().gen_range(0..total);
        for (function, weight) in self.guzzler_fn_weights.buckets() {
            if pick < weight {
                return Ok(function);
            }
            pick -= weight;
        }
        Ok(GuzzlerFunction::HashLoop)
    }

    fn select_erc20_function(&self) -> Result<Erc20Function> {
        let total = self.erc20_fn_weights.total_weight();
        if total == 0 {
            eyre::bail!("select_erc20_function called with total weight 0");
        }
        let mut pick = rand::thread_rng().gen_range(0..total);
        for (function, weight) in self.erc20_fn_weights.buckets() {
            if pick < weight {
                return Ok(function);
            }
            pick -= weight;
        }
        Ok(Erc20Function::Transfer)
    }

    fn select_tx_type(&self) -> Result<TxType> {
        let total = self.tx_type_mix.total_weight();
        if total == 0 {
            eyre::bail!("select_tx_type called with total weight 0");
        }
        let mut pick = rand::thread_rng().gen_range(0..total);
        for (tx_type, weight) in self.tx_type_mix.buckets() {
            if pick < weight {
                return Ok(tx_type);
            }
            pick -= weight;
        }
        Ok(TxType::Transfer)
    }

    /// When set, `init()` will eagerly query the latest nonce for every
    /// account before the first transaction is generated.
    pub fn with_query_nonces_on_init(mut self, enabled: bool) -> Self {
        self.query_nonces_on_init = enabled;
        self
    }

    async fn build_ws_clients(&self) -> Result<Vec<WsClient>> {
        let mut ws_clients = Vec::new();
        for builder in self.ws_client_builders.iter().cloned() {
            ws_clients.push(builder.build().await?);
        }
        Ok(ws_clients)
    }

    // Initialize a range of signer accounts in parallel
    pub async fn initialize_accounts(
        &mut self,
        account_builder: &AccountBuilder,
        signers_range: Range<usize>,
        query_latest_nonce: bool,
    ) -> Result<()> {
        let size = signers_range.len();

        // Spawn tasks to initialize accounts in parallel
        let mut handles = Vec::new();
        for i in 0..size {
            let mut ws_clients = self.build_ws_clients().await?;

            let account_builder = account_builder.clone();
            let signers_range = signers_range.clone();
            let signer = account_builder.build(signers_range.start + i)?;
            handles.push(tokio::spawn(async move {
                let address = signer.address();
                let nonce = if query_latest_nonce {
                    TxGenerator::get_latest_nonce(&mut ws_clients, address)
                        .await
                        .unwrap_or_else(|e| {
                            warn!("Failed to get latest nonce from {address}: {e}");
                            0
                        })
                } else {
                    0
                };
                (signer, Some(nonce))
            }));
        }

        // Collect results
        let mut signers = Vec::with_capacity(size);
        let mut next_nonces = Vec::with_capacity(size);
        for handle in handles.into_iter() {
            let (signer, nonce) = handle.await?;
            signers.push(Some(signer));
            next_nonces.push(nonce);
        }

        self.signers = signers;
        self.next_nonces = next_nonces;

        Ok(())
    }

    /// Lazily build WS clients, verify GasGuzzler deployment if needed, and
    /// optionally query the latest nonce for every account.
    pub async fn init(&mut self) -> Result<()> {
        if self.ws_clients.is_none() {
            self.ws_clients = Some(self.build_ws_clients().await?);
        }
        if self.tx_type_mix.guzzler > 0 && !self.guzzler_verified {
            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized above");
            if !Self::is_contract_deployed(ws_clients, GUZZLER_ADDRESS).await {
                eyre::bail!("GasGuzzler contract not found at {GUZZLER_ADDRESS}.");
            }
            info!("GasGuzzler contract verified at {GUZZLER_ADDRESS}");
            self.guzzler_verified = true;
        }
        if self.tx_type_mix.erc20 > 0 && !self.test_token_verified {
            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized above");
            if !Self::is_contract_deployed(ws_clients, TEST_TOKEN_ADDRESS).await {
                eyre::bail!("TestToken contract not found at {TEST_TOKEN_ADDRESS}.");
            }
            info!("TestToken contract verified at {TEST_TOKEN_ADDRESS}");
            self.test_token_verified = true;
        }
        if self.query_nonces_on_init {
            self.query_nonces_on_init = false; // run once
            self.query_all_nonces().await?;
        }
        Ok(())
    }

    /// Query the latest nonce for every account that hasn't been initialized yet.
    async fn query_all_nonces(&mut self) -> Result<()> {
        info!(
            "TxGenerator {}: querying latest nonces for {} accounts...",
            self.id,
            self.signers_range.len()
        );
        for i in 0..self.signers_range.len() {
            if self.next_nonces[i].is_some() {
                continue;
            }
            // Build signer if needed (to get the address)
            if self.signers[i].is_none() {
                let index = self.signers_range.start + i;
                self.signers[i] = Some(self.account_builder.build(index)?);
            }
            let address = self.signers[i]
                .as_ref()
                .expect("signer built above")
                .address();
            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized by init()");
            let nonce = Self::get_latest_nonce(ws_clients, address)
                .await
                .unwrap_or_else(|e| {
                    warn!("Failed to get latest nonce for {address}: {e}");
                    0
                });
            self.next_nonces[i] = Some(nonce);
        }
        info!("TxGenerator {}: nonce query complete", self.id);
        Ok(())
    }

    /// Generate and sign the next transaction in round-robin order.
    ///
    /// Returns `Some((signed_tx, account_index))` or `None` when all accounts
    /// have hit `max_txs_per_account`. The nonce is NOT incremented; the
    /// caller must call `ack_nonce(account_index)` after the transaction is
    /// accepted.
    pub async fn next_tx(&mut self) -> Result<Option<(Signed<TxEip1559>, usize)>> {
        self.init().await?;

        let num_accounts = self.signers.len();
        // Try each account once, starting from next_account_index
        let mut tried = 0;
        while tried < num_accounts {
            let i = self.next_account_index % num_accounts;
            self.next_account_index = (self.next_account_index + 1) % num_accounts;
            tried += 1;

            // Skip exhausted or permanently failed accounts
            if self.skipped_accounts.contains(&i) {
                continue;
            }
            // max_txs_per_account == 0 implies unlimited
            if self.max_txs_per_account > 0 && self.tx_counts[i] >= self.max_txs_per_account {
                continue;
            }

            // Resolve all config values before borrowing ws_clients mutably.
            let tx_type = self.select_tx_type()?;
            let guzzler_selection = if matches!(tx_type, TxType::Guzzler) {
                let func = self.select_guzzler_function()?;
                Some((func, self.guzzler_fn_weights.arg_for(func)))
            } else {
                None
            };
            let erc20_function = if matches!(tx_type, TxType::Erc20) {
                Some(self.select_erc20_function()?)
            } else {
                None
            };

            // For ERC-20, resolve the recipient address before ws_clients borrow.
            // With multiple accounts, use the next account in round-robin order.
            // With a single account, use a deterministic address to avoid self-transfer.
            let erc20_recipient = if matches!(tx_type, TxType::Erc20) {
                if num_accounts > 1 {
                    let recipient_index = (i + 1) % num_accounts;
                    Some(self.ensure_signer(recipient_index)?.address())
                } else {
                    Some(Address::left_padding_from(&[0xEC, 0x20]))
                }
            } else {
                None
            };

            // Ensure the current signer is initialized.
            let signer_addr = self.ensure_signer(i)?.address();

            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized by init()");

            // Initialize nonce
            let next_nonce = match self.next_nonces[i] {
                Some(nonce) => nonce,
                None => {
                    if self.query_latest_nonce {
                        TxGenerator::get_latest_nonce(ws_clients, signer_addr)
                            .await
                            .unwrap_or_else(|e| {
                                warn!("Failed to get latest nonce from {signer_addr}: {e}");
                                0
                            })
                    } else {
                        0
                    }
                }
            };

            // Generate tx based on selected type
            let tx = match tx_type {
                TxType::Transfer => self.make_eip1559_tx(next_nonce),
                TxType::Erc20 => {
                    let recipient = erc20_recipient.expect("resolved above for TxType::Erc20");
                    let function =
                        erc20_function.expect("erc20_function resolved above for TxType::Erc20");
                    crate::erc20::prepare_erc20_tx(
                        ws_clients,
                        signer_addr,
                        recipient,
                        next_nonce,
                        function,
                    )
                    .await?
                }
                TxType::Guzzler => {
                    let (guzzler_function, base_arg) =
                        guzzler_selection.expect("guzzler_selection set for TxType::Guzzler");
                    Self::prepare_guzzler_call_tx(
                        ws_clients,
                        signer_addr,
                        GUZZLER_ADDRESS,
                        next_nonce,
                        base_arg,
                        guzzler_function,
                    )
                    .await?
                }
            };
            let tx_sign_hash = tx.signature_hash();
            let signer = self.signers[i].as_ref().expect("signer initialized above");
            let signature = signer.sign_hash(&tx_sign_hash).await?;
            let signed_tx = tx.into_signed(signature);

            // Store nonce so that a repeated call without ack retries the same nonce
            self.next_nonces[i] = Some(next_nonce);

            return Ok(Some((signed_tx, i)));
        }

        // All accounts exhausted
        Ok(None)
    }

    /// Acknowledge that a transaction for the given account was accepted.
    /// Increments the nonce and per-account tx count.
    pub fn ack_nonce(&mut self, account_index: usize) {
        if let Some(nonce) = self.next_nonces[account_index] {
            self.next_nonces[account_index] = Some(nonce + 1);
        }
        self.tx_counts[account_index] += 1;
    }

    /// Permanently exclude an account from future round-robin iterations.
    ///
    /// Call this when an account has failed `MAX_CONSECUTIVE_FAILURES`
    /// consecutive times for non-nonce reasons (e.g., insufficient funds,
    /// blocklisted address) to avoid infinite retry loops. See
    /// `TxSender::run_backpressure()` for the call site.
    pub fn skip_account(&mut self, account_index: usize) {
        self.skipped_accounts.insert(account_index);
    }

    /// Re-query the latest nonce for the given account from the node.
    /// Used after a rejection to recover to the correct nonce.
    pub async fn refresh_nonce(&mut self, account_index: usize) -> Result<()> {
        let address = self.signers[account_index]
            .as_ref()
            .ok_or_else(|| eyre::eyre!("signer at index {account_index} not initialized"))?
            .address();
        let ws_clients = self
            .ws_clients
            .as_mut()
            .ok_or_else(|| eyre::eyre!("ws_clients not initialized"))?;
        let nonce = Self::get_latest_nonce(ws_clients, address).await?;
        self.next_nonces[account_index] = Some(nonce);
        Ok(())
    }

    /// Generate transactions and send them to the load scheduler (fire-and-forget mode).
    pub async fn run(&mut self) -> Result<()> {
        debug!("TxGenerator {}: running...", self.id);

        let tx_sender = self
            .tx_sender
            .as_ref()
            .ok_or_else(|| eyre::eyre!("run() requires a tx_sender channel"))?
            .clone();

        loop {
            match self.next_tx().await? {
                Some((signed_tx, account_index)) => {
                    if tx_sender.send(signed_tx).await.is_err() {
                        // Channel closed, abort
                        return Ok(());
                    }
                    // Fire-and-forget: optimistically ack nonce after channel push
                    self.ack_nonce(account_index);
                }
                None => {
                    // All accounts exhausted
                    return Ok(());
                }
            }
        }
    }

    /// Ensure a signer is initialized at the given index, returning a reference.
    fn ensure_signer(&mut self, index: usize) -> Result<&LocalSigner<SigningKey>> {
        if self.signers[index].is_none() {
            let account_index = self.signers_range.start + index;
            self.signers[index] = Some(self.account_builder.build(account_index)?);
        }
        Ok(self.signers[index]
            .as_ref()
            .expect("signer initialized above"))
    }

    /// Prepare a GasGuzzler call tx: adjust argument, estimate gas, and build the tx
    async fn prepare_guzzler_call_tx(
        ws_clients: &mut [WsClient],
        signer_addr: Address,
        contract_addr: Address,
        nonce: u64,
        base_arg: u64,
        guzzler_function: GuzzlerFunction,
    ) -> Result<TxEip1559> {
        let factor: u64 = rand::thread_rng().gen_range(80u64..=120u64); // -/+ 20% random adjustment
        let adjusted_arg = core::cmp::max(1, base_arg.saturating_mul(factor) / 100);
        let calldata = Self::encode_guzzler_calldata(guzzler_function, adjusted_arg);
        let estimate =
            Self::estimate_gas_tx(ws_clients, signer_addr, Some(contract_addr), &calldata).await;
        let gas_limit = estimate
            .map(|g| g.saturating_mul(5) / 4) // 25% safety margin
            .unwrap_or(10_000_000); // fall back to a generous limit
        Ok(Self::make_guzzler_call_tx(
            nonce,
            contract_addr,
            adjusted_arg,
            guzzler_function,
            gas_limit,
        ))
    }

    /// Check if contract code exists at address.
    async fn is_contract_deployed(ws_clients: &mut [WsClient], address: Address) -> bool {
        for ws_client in ws_clients.iter_mut() {
            if let Ok(code_hex) = ws_client
                .request_response::<String>("eth_getCode", json!([address, "latest"]))
                .await
            {
                // Non-empty code is anything other than "0x" or "0x0"
                let code = code_hex.trim();
                if code != "0x" && code != "0x0" {
                    return true;
                }
            }
        }
        false
    }

    fn encode_guzzler_calldata(guzzler_function: GuzzlerFunction, arg: u64) -> Bytes {
        sol! {
            function hashLoop(uint256 iterations);
            function storageWrite(uint256 iterations);
            function storageRead(uint256 iterations);
            function guzzle(uint256 gasRemaining);
            function guzzle2(uint256 gasRemaining);
        }
        let arg = U256::from(arg);
        match guzzler_function {
            GuzzlerFunction::HashLoop => hashLoopCall { iterations: arg }.abi_encode().into(),
            GuzzlerFunction::StorageWrite => {
                storageWriteCall { iterations: arg }.abi_encode().into()
            }
            GuzzlerFunction::StorageRead => storageReadCall { iterations: arg }.abi_encode().into(),
            GuzzlerFunction::Guzzle => guzzleCall { gasRemaining: arg }.abi_encode().into(),
            GuzzlerFunction::Guzzle2 => guzzle2Call { gasRemaining: arg }.abi_encode().into(),
        }
    }

    /// Estimate gas for a transaction.
    pub(crate) async fn estimate_gas_tx(
        ws_clients: &mut [WsClient],
        from: Address,
        to: Option<Address>,
        data: &Bytes,
    ) -> Option<u64> {
        for ws_client in ws_clients.iter_mut() {
            let mut tx = serde_json::Map::new();
            tx.insert("from".to_string(), json!(from));
            if let Some(to_addr) = to {
                tx.insert("to".to_string(), json!(to_addr));
            }
            tx.insert("data".to_string(), json!(data));
            tx.insert("value".to_string(), json!("0x0"));

            let params = json!([tx]);
            match ws_client
                .request_response::<String>("eth_estimateGas", params)
                .await
            {
                Ok(resp) => {
                    let hex_str = resp.trim_start_matches("0x");
                    if let Ok(v) = u64::from_str_radix(hex_str, 16) {
                        return Some(v);
                    }
                }
                Err(_) => continue,
            }
        }
        None
    }

    /// Query all RPC endpoints to find the latest nonce (the highest
    /// value) used by the given address. Tolerates individual node
    /// failures and returns the highest nonce from any successful
    /// response. Returns an error only if *all* nodes fail.
    async fn get_latest_nonce(ws_clients: &mut [WsClient], address: Address) -> Result<u64> {
        let mut highest_nonce: Option<u64> = None;

        for ws_client in ws_clients.iter_mut() {
            match ws_client
                .request_response::<String>("eth_getTransactionCount", json!([address, "pending"]))
                .await
            {
                Ok(response) => {
                    let hex_str = response.strip_prefix("0x").unwrap_or(&response);
                    match u64::from_str_radix(hex_str, 16) {
                        Ok(nonce) => {
                            highest_nonce = Some(highest_nonce.map_or(nonce, |h| h.max(nonce)));
                        }
                        Err(e) => {
                            warn!(
                                "Bad nonce response for {address} from {}: '{response}': {e}",
                                ws_client.url,
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to query nonce for {address} from {}: {e}",
                        ws_client.url,
                    );
                }
            }
        }

        highest_nonce.ok_or_else(|| eyre::eyre!("all nodes failed to return nonce for {address}"))
    }

    /// Create a new EIP-1559 transaction.
    fn make_eip1559_tx(&self, nonce: u64) -> TxEip1559 {
        let input = Bytes::from(vec![0u8; self.tx_input_size]);
        let input_gas = input.len() as u64 * 16;

        TxEip1559 {
            chain_id: TESTNET_CHAIN_ID,
            nonce,
            max_priority_fee_per_gas: 1_000_000_000, // 1 gwei
            max_fee_per_gas: 2_000_000_000,          // 2 gwei
            gas_limit: 30_000 + input_gas, // base tx + input gas, Arc requires ~26k for transfers (blocklist check)
            to: Address::left_padding_from(&(nonce.wrapping_add(0x1000)).to_be_bytes()).into(), // avoid zero address and Ethereum precompile addresses
            value: U256::from(1e16), // 0.01 ETH
            input,
            access_list: Default::default(),
        }
    }

    /// Create an EIP-1559 tx that calls the selected GasGuzzler function.
    fn make_guzzler_call_tx(
        nonce: u64,
        addr: Address,
        arg: u64,
        guzzler_function: GuzzlerFunction,
        gas_limit: u64,
    ) -> TxEip1559 {
        let input = Self::encode_guzzler_calldata(guzzler_function, arg);
        TxEip1559 {
            chain_id: TESTNET_CHAIN_ID,
            nonce,
            max_priority_fee_per_gas: 1_000_000_000, // 1 gwei
            max_fee_per_gas: 2_000_000_000,          // 2 gwei
            gas_limit,
            to: Some(addr).into(),
            value: U256::ZERO,
            input,
            access_list: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spammer::TEST_MNEMONIC;
    use std::{collections::HashMap, time::Duration};
    use tokio::sync::mpsc;

    fn make_generator(
        start: usize,
        end: usize,
        tx_sender: Option<Sender<Signed<TxEip1559>>>,
        max_txs_per_account: u64,
    ) -> TxGenerator {
        let account_builder = AccountBuilder::new(TEST_MNEMONIC.to_string());
        TxGenerator::new(
            0,
            start..end,
            account_builder,
            vec![],
            tx_sender,
            max_txs_per_account,
            false,
            0,
            GuzzlerFnWeights::default(),
            Erc20FnWeights {
                transfer: 100,
                ..Default::default()
            },
            TxTypeMix {
                transfer: 100,
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn tx_generator_distributes_across_signers() -> Result<()> {
        let account_builder = AccountBuilder::new(TEST_MNEMONIC.to_string());

        #[rustfmt::skip]
        let test_cases = vec![
            (0, 10, 10),
            (10, 15, 10),
            (10, 11, 10),
            (10, 20, 40),
            (0, 100, 50),
            (900, 1000, 1000),
        ];
        for (start, end, channel_capacity) in test_cases {
            let (tx_sender, mut tx_receiver) = mpsc::channel::<Signed<TxEip1559>>(channel_capacity);
            let mut generator = make_generator(start, end, Some(tx_sender), 0);

            // When we run the generator briefly to fill up the channel
            let handle = tokio::spawn(async move { generator.run().await });
            tokio::time::sleep(Duration::from_millis(channel_capacity as u64)).await;
            handle.abort(); // to stop producing more txs
            let _ = handle.await; // ignore join errors from abort

            // Drain generated txs from channel and count txs per signer (by recovered sender address)
            let mut per_sender_counts: HashMap<Address, usize> = HashMap::new();
            let mut counter = 0usize;
            while let Ok(signed) = tx_receiver.try_recv() {
                // Recover sender address from signed transaction
                let sighash = signed.signature_hash();
                let sender = signed
                    .signature()
                    .recover_address_from_prehash(&sighash)
                    .expect("recover signer");

                *per_sender_counts.entry(sender).or_default() += 1;
                counter += 1;
            }

            // Then all generated txs were sent to the channel
            assert!(
                counter <= channel_capacity,
                "expected at most {channel_capacity} generated transactions"
            );

            // Build expected distribution: round-robin 1 tx per signer
            let signers = (start..end)
                .map(|index| account_builder.build(index))
                .collect::<Result<Vec<_>>>()?;
            let signer_addresses: Vec<Address> = signers.iter().map(|s| s.address()).collect();
            let mut expected: HashMap<Address, usize> = HashMap::new();
            let num_signers = end - start;
            for i in 0..counter {
                let idx = i % num_signers;
                *expected.entry(signer_addresses[idx]).or_default() += 1;
            }

            assert_eq!(
                per_sender_counts, expected,
                "per-signer counts should match round-robin distribution"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn next_tx_without_ack_retries_same_nonce() -> Result<()> {
        let mut generator = make_generator(0, 1, None, 0);

        let (tx1, idx1) = generator.next_tx().await?.expect("first tx");
        // Do NOT ack — next call should produce same nonce
        let (tx2, idx2) = generator.next_tx().await?.expect("retry tx");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 0);
        assert_eq!(
            tx1.tx().nonce,
            tx2.tx().nonce,
            "nonce should be unchanged without ack"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ack_nonce_increments() -> Result<()> {
        let mut generator = make_generator(0, 1, None, 0);

        let (tx1, idx) = generator.next_tx().await?.expect("first tx");
        generator.ack_nonce(idx);
        let (tx2, _) = generator.next_tx().await?.expect("second tx");

        assert_eq!(
            tx2.tx().nonce,
            tx1.tx().nonce + 1,
            "nonce should increment after ack"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_tx_respects_max_txs_per_account() -> Result<()> {
        let max_txs = 3;
        let mut generator = make_generator(0, 2, None, max_txs);

        // Generate and ack max_txs for each account = 2 * 3 = 6 total
        let mut count = 0u64;
        while let Some((_, idx)) = generator.next_tx().await? {
            generator.ack_nonce(idx);
            count += 1;
            if count > 100 {
                panic!("too many txs generated");
            }
        }

        assert_eq!(
            count,
            2 * max_txs,
            "should produce max_txs_per_account for each account"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_tx_round_robin_distribution() -> Result<()> {
        let num_accounts = 5;
        let num_txs = 15u64;
        let mut generator = make_generator(0, num_accounts, None, 0);

        let mut per_account: HashMap<usize, u64> = HashMap::new();
        for _ in 0..num_txs {
            let (_, idx) = generator.next_tx().await?.expect("tx");
            *per_account.entry(idx).or_default() += 1;
            generator.ack_nonce(idx);
        }

        // Each account should get exactly num_txs / num_accounts = 3
        for i in 0..num_accounts {
            assert_eq!(
                per_account.get(&i).copied().unwrap_or(0),
                num_txs / num_accounts as u64,
                "account {i} should get equal share"
            );
        }
        Ok(())
    }
}
