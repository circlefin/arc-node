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

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope, TxLegacy};
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_signer::Signer;
use alloy_signer_local::LocalSigner;
use alloy_sol_types::{sol, SolCall};
use color_eyre::eyre::{self, Result};
use k256::ecdsa::SigningKey;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use tokio::sync::mpsc::{error::TryRecvError, Receiver, Sender};
use tracing::{debug, info, warn};

/// Outcome of a single fire-and-forget tx submission as observed by the sender.
///
/// Sent back from `TxSender` to `TxGenerator` over a dedicated channel so the
/// generator can refresh the cached nonce on rejection. `Accepted` doesn't
/// carry an account index because the generator already optimistically
/// advanced its cache at submit time — the variant is kept so the sender
/// can still emit "successfully observed" outcomes for diagnostic logging
/// without forcing every caller to discriminate between "didn't drain" and
/// "drained Ok".
#[derive(Debug, Clone, Copy)]
pub(crate) enum AckOutcome {
    /// Node accepted the tx into its mempool — no-op for the generator
    /// (cache was already advanced optimistically).
    Accepted,
    /// Node rejected the tx (any failure, transient or terminal). The
    /// payload is the **account index** whose tx was rejected; the generator
    /// uses it to refresh that account's cached nonce from chain so the next
    /// submission for that account uses the correct value, which corrects
    /// any optimistic overshoot.
    Rejected(usize),
}

use crate::accounts::AccountBuilder;
use crate::config::{
    Erc20FnWeights, Erc20Function, GuzzlerFnWeights, GuzzlerFunction, TxType, TxTypeMix,
};
use crate::ws::{WsClient, WsClientBuilder};

use crate::erc20::TEST_TOKEN_ADDRESS;

pub(crate) const TESTNET_CHAIN_ID: u64 = 1337;

/// Max fee per gas (in wei) used for all generated transactions.
///
/// Sized for 2x headroom over the testnet `maxBaseFee` ceiling (20,000 gwei) so
/// the spammer keeps submitting when the base fee pegs at the ceiling.
pub(crate) const MAX_FEE_PER_GAS: u128 = 40_000 * 1_000_000_000;

/// Max priority fee per gas (tip) in wei.
pub(crate) const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;

const GUZZLER_ADDRESS: Address = address!("45a834A6bB86F516D4157a8cBcc60f2F35F8398C");

/// Fallback gas limit for Guzzler calls when estimation fails.
const GUZZLER_GAS_FALLBACK: u64 = 10_000_000;

/// Multiplier applied to cached gas estimates (covers per-call arg variance across all
/// future transactions of that type).
pub(crate) const GAS_CACHE_BUFFER: u64 = 2;

/// Numerator and denominator for the safety margin applied to per-call gas estimates
/// on the fallback path (1.25×).
pub(crate) const GAS_ESTIMATE_MARGIN_NUM: u64 = 5;
pub(crate) const GAS_ESTIMATE_MARGIN_DEN: u64 = 4;

/// Number of parallel workers used by `resync_nonces` to fan out per-account
/// nonce queries. Each worker holds its own WS client set, so the total
/// connection count per node is `RESYNC_CONCURRENCY × num_builders`, kept
/// small enough to avoid the HTTP 429 storm that motivated the original
/// single-WS-client design (50k accounts × N builders would otherwise open
/// 50k+ parallel connections).
const RESYNC_CONCURRENCY: usize = 8;

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
#[derive(Serialize, Deserialize)]
pub(crate) struct TxGenerator {
    id: usize,
    /// BIP32-derived signing keys. Skipped on (de)serialise — keys are a
    /// deterministic function of `account_builder.mnemonic` + account index,
    /// so the lazy `if signers[i].is_none() { build }` path in `next_tx`
    /// reconstructs them on first use. `ensure_signers_capacity` is called
    /// from `Spammer::new_resuming` to size the Vec to `signers_range.len()`
    /// before any per-account access happens.
    #[serde(skip, default)]
    signers: Vec<Option<LocalSigner<SigningKey>>>,
    signers_range: Range<usize>,
    next_nonces: Vec<Option<u64>>,
    account_builder: AccountBuilder,
    /// Skipped — repopulated by `update_ws_client_builders` on resume.
    #[serde(skip, default)]
    ws_client_builders: Vec<WsClientBuilder>,
    /// Channel to send `(signed_tx, account_index)` pairs to a separate
    /// `TxSender` task (fire-and-forget mode). The account_index is plumbed
    /// through so the sender can correlate ack/refresh outcomes back to a
    /// specific account via the per-client inflight deques.
    /// `None` in backpressure mode, where the sender owns the generator directly.
    /// Skipped — repopulated by `reset_tx_sender` on resume.
    #[serde(skip, default)]
    tx_sender: Option<Sender<(TxEnvelope, usize)>>,
    max_txs_per_account: u64,
    query_latest_nonce: bool,
    tx_input_size: usize,
    guzzler_fn_weights: GuzzlerFnWeights,
    erc20_fn_weights: Erc20FnWeights,
    tx_type_mix: TxTypeMix,
    /// Lazily built WS clients (used by next_tx for guzzler gas estimation and nonce queries).
    /// Skipped — rebuilt on demand from `ws_client_builders` on resume.
    #[serde(skip, default)]
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
    /// Cached gas limit per ERC-20 function, estimated once in init() with 2× buffer.
    erc20_gas_cache: HashMap<Erc20Function, u64>,
    /// Cached gas limit per Guzzler function, estimated once in init() with 2× buffer.
    guzzler_gas_cache: HashMap<GuzzlerFunction, u64>,
}

impl TxGenerator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: usize,
        signers_range: Range<usize>,
        account_builder: AccountBuilder,
        ws_client_builders: Vec<WsClientBuilder>,
        tx_sender: Option<Sender<(TxEnvelope, usize)>>,
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
            erc20_gas_cache: HashMap::new(),
            guzzler_gas_cache: HashMap::new(),
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

    /// Replace the tx channel sender, used when reusing a generator across phases.
    pub(crate) fn reset_tx_sender(&mut self, sender: Sender<(TxEnvelope, usize)>) {
        self.tx_sender = Some(sender);
        // Clear stale connections so init() rebuilds them on the next run.
        self.ws_clients = None;
    }

    /// Update the WebSocket client builders, used when reusing a generator
    /// against a different set of target nodes. Clears cached connections so
    /// they are rebuilt against the new URLs on the next run.
    pub(crate) fn update_ws_client_builders(&mut self, builders: Vec<WsClientBuilder>) {
        self.ws_client_builders = builders;
        self.ws_clients = None;
    }

    /// Size the `signers` Vec to `signers_range.len()`, inserting `None`
    /// placeholders. Called after deserialising a persisted
    /// [`SpammerState`](crate::SpammerState) so the lazy per-account
    /// derivation in `next_tx`/`next_signed_tx` has indexable slots to fill
    /// on first use. No-op for in-process resumes where `signers` is already
    /// populated.
    pub(crate) fn ensure_signers_capacity(&mut self) {
        let size = self.signers_range.len();
        if self.signers.len() != size {
            self.signers = vec![None; size];
        }
    }

    /// True when every account in `signers_range` already has a derived key.
    /// Used by [`SpammerState::eagerly_derive_signers`] to short-circuit the
    /// `spawn_blocking` fan-out for in-process resumes.
    pub(crate) fn signers_populated(&self) -> bool {
        self.signers.len() == self.signers_range.len() && self.signers.iter().all(Option::is_some)
    }

    /// Pre-derive every BIP32 signing key for this generator's account range,
    /// matching the warm-cache that local-mode `SpammerState` has after
    /// ramp-up. Pure CPU work; called via `tokio::task::spawn_blocking` from
    /// [`SpammerState::eagerly_derive_signers`] so the runtime can parallelise
    /// across generators. No-op when keys are already populated.
    pub(crate) fn eagerly_derive_signers(&mut self) -> Result<()> {
        if self.signers_populated() {
            return Ok(());
        }
        let size = self.signers_range.len();
        let start = self.signers_range.start;
        let mut signers = Vec::with_capacity(size);
        for i in 0..size {
            signers.push(Some(self.account_builder.build(start + i)?));
        }
        self.signers = signers;
        Ok(())
    }

    async fn build_ws_clients(&self) -> Result<Vec<WsClient>> {
        let mut ws_clients = Vec::with_capacity(self.ws_client_builders.len());
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
        let erc20_cache_incomplete = self
            .erc20_fn_weights
            .buckets()
            .iter()
            .any(|(f, w)| *w > 0 && !self.erc20_gas_cache.contains_key(f));
        if self.tx_type_mix.erc20 > 0 && erc20_cache_incomplete {
            let from = self.first_account_address()?;
            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized above");
            for (function, weight) in self.erc20_fn_weights.buckets() {
                if weight == 0 {
                    continue;
                }
                let calldata = match function {
                    Erc20Function::Transfer => {
                        crate::erc20::encode_transfer(from, U256::from(1u64))
                    }
                    Erc20Function::Approve => crate::erc20::encode_approve(from, U256::from(1u64)),
                    Erc20Function::TransferFrom => {
                        crate::erc20::encode_transfer_from(from, from, U256::from(1u64))
                    }
                };
                match Self::estimate_gas_tx(ws_clients, from, Some(TEST_TOKEN_ADDRESS), &calldata)
                    .await
                {
                    Some(estimate) => {
                        self.erc20_gas_cache
                            .insert(function, estimate.saturating_mul(GAS_CACHE_BUFFER));
                    }
                    None => warn!(
                        "gas estimate failed for ERC-20 {:?}; falling back to per-tx estimation",
                        function
                    ),
                }
            }
        }
        let guzzler_cache_incomplete = self
            .guzzler_fn_weights
            .buckets()
            .iter()
            .any(|(f, w)| *w > 0 && !self.guzzler_gas_cache.contains_key(f));
        if self.tx_type_mix.guzzler > 0 && guzzler_cache_incomplete {
            let from = self.first_account_address()?;
            let ws_clients = self
                .ws_clients
                .as_mut()
                .expect("ws_clients initialized above");
            for (function, weight) in self.guzzler_fn_weights.buckets() {
                if weight == 0 {
                    continue;
                }
                let base_arg = self.guzzler_fn_weights.arg_for(function);
                let calldata = TxGenerator::encode_guzzler_calldata(function, base_arg);
                match Self::estimate_gas_tx(ws_clients, from, Some(GUZZLER_ADDRESS), &calldata)
                    .await
                {
                    Some(estimate) => {
                        self.guzzler_gas_cache
                            .insert(function, estimate.saturating_mul(GAS_CACHE_BUFFER));
                    }
                    None => warn!(
                        "gas estimate failed for Guzzler {:?}; falling back to per-tx estimation",
                        function
                    ),
                }
            }
        }
        if self.query_nonces_on_init {
            self.query_nonces_on_init = false; // run once
            self.query_all_nonces().await?;
        }
        Ok(())
    }

    fn first_account_address(&mut self) -> Result<Address> {
        if self.signers[0].is_none() {
            self.signers[0] = Some(self.account_builder.build(self.signers_range.start)?);
        }
        Ok(self.signers[0].as_ref().expect("built above").address())
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
    pub async fn next_tx(&mut self) -> Result<Option<(TxEnvelope, usize)>> {
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

            // Build, sign, wrap
            let signer = self.signers[i].as_ref().expect("signer initialized above");
            let envelope = match tx_type {
                TxType::Legacy => {
                    let tx = self.make_legacy_tx(next_nonce);
                    let sig = signer.sign_hash(&tx.signature_hash()).await?;
                    TxEnvelope::Legacy(tx.into_signed(sig))
                }
                TxType::Transfer => {
                    let tx = self.make_eip1559_tx(next_nonce);
                    let sig = signer.sign_hash(&tx.signature_hash()).await?;
                    TxEnvelope::Eip1559(tx.into_signed(sig))
                }
                TxType::Erc20 => {
                    let recipient = erc20_recipient.expect("resolved above for TxType::Erc20");
                    let function =
                        erc20_function.expect("erc20_function resolved above for TxType::Erc20");
                    let tx = crate::erc20::prepare_erc20_tx(
                        ws_clients,
                        signer_addr,
                        recipient,
                        next_nonce,
                        function,
                        self.erc20_gas_cache.get(&function).copied(),
                    )
                    .await?;
                    let sig = signer.sign_hash(&tx.signature_hash()).await?;
                    TxEnvelope::Eip1559(tx.into_signed(sig))
                }
                TxType::Guzzler => {
                    let (guzzler_function, base_arg) =
                        guzzler_selection.expect("guzzler_selection set for TxType::Guzzler");
                    let tx = Self::prepare_guzzler_call_tx(
                        ws_clients,
                        signer_addr,
                        GUZZLER_ADDRESS,
                        next_nonce,
                        base_arg,
                        guzzler_function,
                        self.guzzler_gas_cache.get(&guzzler_function).copied(),
                    )
                    .await?;
                    let sig = signer.sign_hash(&tx.signature_hash()).await?;
                    TxEnvelope::Eip1559(tx.into_signed(sig))
                }
            };

            // Store nonce so that a repeated call without ack retries the same nonce
            self.next_nonces[i] = Some(next_nonce);

            return Ok(Some((envelope, i)));
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

    /// Re-query on-chain nonces for all accounts and overwrite cached values.
    ///
    /// Uses `eth_getTransactionCount` with "pending" to skip nonces already
    /// accepted by the pool. The account range is partitioned across
    /// `RESYNC_CONCURRENCY` worker tasks, each owning its own dedicated set
    /// of WS clients — total connections per node stay bounded
    /// (`RESYNC_CONCURRENCY × num_builders`, e.g. 8 × 10 = 80) while restoring
    /// N-way parallelism inside resync. Per-account queries within a worker
    /// remain serial (one in-flight per WS client).
    pub(crate) async fn resync_nonces(&mut self) -> Result<()> {
        let size = self.signers_range.len();
        info!(
            "TxGenerator {}: resyncing nonces for {size} accounts...",
            self.id
        );

        for i in 0..size {
            if self.signers[i].is_none() {
                let index = self.signers_range.start + i;
                self.signers[i] = Some(self.account_builder.build(index)?);
            }
        }

        let workers = RESYNC_CONCURRENCY.min(size).max(1);
        let mut worker_clients = Vec::with_capacity(workers);
        for _ in 0..workers {
            worker_clients.push(self.build_ws_clients().await?);
        }

        let chunk = size.div_ceil(workers);
        let mut handles = Vec::with_capacity(workers);
        for (w, mut clients) in worker_clients.into_iter().enumerate() {
            let start = (w * chunk).min(size);
            let end = ((w + 1) * chunk).min(size);
            if start >= end {
                continue;
            }
            let addresses: Vec<(usize, Address, Option<u64>)> = (start..end)
                .map(|i| {
                    (
                        i,
                        self.signers[i].as_ref().expect("built above").address(),
                        self.next_nonces[i],
                    )
                })
                .collect();
            handles.push(tokio::spawn(async move {
                let mut results = Vec::with_capacity(addresses.len());
                for (i, address, cached) in addresses {
                    let nonce = Self::fetch_nonce(&mut clients, address, cached).await?;
                    results.push((i, nonce));
                }
                Ok::<Vec<(usize, u64)>, eyre::Error>(results)
            }));
        }

        for h in handles {
            for (i, nonce) in h.await?? {
                self.next_nonces[i] = Some(nonce);
            }
        }

        info!("TxGenerator {}: nonce resync complete", self.id);
        Ok(())
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
        let nonce = Self::fetch_nonce(ws_clients, address, None).await?;
        self.next_nonces[account_index] = Some(nonce);
        Ok(())
    }

    /// Generate transactions and send them to the load scheduler (fire-and-forget mode).
    ///
    /// Nonces are still advanced optimistically right after the channel push:
    /// without that, the generator's tight loop would build several txs at
    /// the same nonce before the sender's first response came back, and the
    /// chain would reject every duplicate as `"nonce too low"`. The
    /// `ack_rx` channel from the paired `TxSender` is used purely as a
    /// *correction* signal — `AckOutcome::Rejected` triggers a chain-side
    /// `refresh_nonce` for the affected account, which catches the cases the
    /// optimistic ack overshoots (response-stream errors like
    /// `"nonce too low"`, `"txpool is full"`, validation failures, etc.).
    /// `AckOutcome::Accepted` is therefore a no-op — the cache was already
    /// advanced at submit time.
    pub async fn run(&mut self, mut ack_rx: Receiver<AckOutcome>) -> Result<()> {
        debug!("TxGenerator {}: running...", self.id);

        let tx_sender = self
            .tx_sender
            .as_ref()
            .ok_or_else(|| eyre::eyre!("run() requires a tx_sender channel"))?
            .clone();

        loop {
            // Drain any pending acks (just refreshes — accepts are no-ops)
            // before generating the next tx. Done synchronously via
            // `try_recv` to keep the borrow checker happy — `next_tx().await`
            // borrows `&mut self` mutably, and we need the same borrow
            // inside the refresh handler, so the two cannot overlap in a
            // `select!` arm.
            self.process_pending_acks(&mut ack_rx).await;

            match self.next_tx().await? {
                Some((signed_tx, account_index)) => {
                    if tx_sender.send((signed_tx, account_index)).await.is_err() {
                        // Channel closed, abort
                        return Ok(());
                    }
                    // Optimistic ack: advance cache so the next iteration's
                    // `next_tx` uses nonce+1. Sender-driven refresh on
                    // rejection corrects the overshoot if the tx is rejected.
                    self.ack_nonce(account_index);
                }
                None => {
                    // All accounts exhausted — drain any remaining acks so
                    // late-arriving rejections still trigger refresh.
                    self.process_pending_acks(&mut ack_rx).await;
                    return Ok(());
                }
            }
        }
    }

    /// Drain whatever `AckOutcome` messages are immediately available.
    /// `Accepted` is a no-op (the generator already advanced the cache
    /// optimistically at submit time). `Rejected` flags the account as
    /// dirty so any overshoot is corrected before the next tx for that
    /// account is built. Refreshes happen after the drain completes,
    /// deduplicated per account — a burst of inflight rejections for the
    /// same account (e.g. several stale-nonce txs queued behind a real
    /// rejection) costs one chain query, not N.
    /// Non-blocking: returns as soon as `try_recv` yields `Empty`. A
    /// `Disconnected` channel is treated as terminal — the sender side
    /// has exited and there's nothing more to ack.
    async fn process_pending_acks(&mut self, ack_rx: &mut Receiver<AckOutcome>) {
        let mut dirty: HashSet<usize> = HashSet::new();
        loop {
            match ack_rx.try_recv() {
                Ok(AckOutcome::Accepted) => {}
                Ok(AckOutcome::Rejected(idx)) => {
                    dirty.insert(idx);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        for idx in dirty {
            // If the chain query fails, leave the cached (advanced) nonce in
            // place and log. The account will keep emitting txs with the
            // stale optimistic nonce until the next rejection drives another
            // refresh attempt. Worst case it converges at the next phase-end
            // resync, which re-queries every account from chain.
            if let Err(e) = self.refresh_nonce(idx).await {
                debug!(
                    "TxGenerator {}: refresh_nonce failed for account {idx}: {e}",
                    self.id
                );
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
        cached_gas: Option<u64>,
    ) -> Result<TxEip1559> {
        let factor: u64 = rand::thread_rng().gen_range(80u64..=120u64); // -/+ 20% random adjustment
        let adjusted_arg = core::cmp::max(1, base_arg.saturating_mul(factor) / 100);
        let calldata = Self::encode_guzzler_calldata(guzzler_function, adjusted_arg);
        let gas_limit = if let Some(cached) = cached_gas {
            cached
        } else {
            let estimate =
                Self::estimate_gas_tx(ws_clients, signer_addr, Some(contract_addr), &calldata)
                    .await;
            estimate
                .map(|g| g.saturating_mul(GAS_ESTIMATE_MARGIN_NUM) / GAS_ESTIMATE_MARGIN_DEN)
                .unwrap_or(GUZZLER_GAS_FALLBACK)
        };
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

    /// Fetch the latest nonce for `address`, falling back to `cached` on failure.
    ///
    /// Pass `cached = None` to propagate errors (e.g. in `refresh_nonce`, where
    /// the caller must obtain the correct nonce). Pass `cached = Some(n)` to
    /// tolerate transient RPC failures by keeping the last known value.
    async fn fetch_nonce(
        ws_clients: &mut [WsClient],
        address: Address,
        cached: Option<u64>,
    ) -> Result<u64> {
        match (Self::get_latest_nonce(ws_clients, address).await, cached) {
            (Ok(nonce), _) => Ok(nonce),
            (Err(e), Some(c)) => {
                warn!("Failed to fetch nonce for {address}: {e}; keeping cached {c}");
                Ok(c)
            }
            (Err(e), None) => Err(e),
        }
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
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            gas_limit: 30_000 + input_gas, // base tx + input gas, Arc requires ~26k for transfers (blocklist check)
            to: Address::left_padding_from(&(nonce.wrapping_add(0x1000)).to_be_bytes()).into(), // avoid zero address and Ethereum precompile addresses
            value: U256::from(1e16), // 0.01 ETH
            input,
            access_list: Default::default(),
        }
    }

    /// Create a legacy (Type 0) value transfer.
    fn make_legacy_tx(&self, nonce: u64) -> TxLegacy {
        let input = Bytes::from(vec![0u8; self.tx_input_size]);
        let input_gas = input.len() as u64 * 16;

        TxLegacy {
            chain_id: Some(TESTNET_CHAIN_ID),
            nonce,
            gas_price: MAX_FEE_PER_GAS,
            gas_limit: 30_000 + input_gas,
            to: Address::left_padding_from(&(nonce.wrapping_add(0x1000)).to_be_bytes()).into(),
            value: U256::from(1e16), // 0.01 ETH
            input,
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
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            max_fee_per_gas: MAX_FEE_PER_GAS,
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
    use alloy_consensus::{transaction::SignerRecoverable, Transaction};
    use std::{collections::HashMap, time::Duration};
    use tokio::sync::mpsc;

    fn make_generator(
        start: usize,
        end: usize,
        tx_sender: Option<Sender<(TxEnvelope, usize)>>,
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

    #[test]
    fn tx_generator_round_trips_through_json() {
        // Build a generator, populate the persistable state, serialise,
        // deserialise, and verify it round-trips. `signers` is `#[serde(skip)]`
        // — keys are re-derived lazily via `next_tx`, with capacity restored
        // by `ensure_signers_capacity` (exercised below).
        let mut tg = make_generator(5, 10, None, 0);
        for i in 0..5 {
            tg.next_nonces[i] = Some(i as u64 + 42);
            tg.tx_counts[i] = (i as u64 + 1) * 7;
        }
        tg.next_account_index = 3;
        tg.skipped_accounts.insert(2);
        tg.guzzler_verified = true;
        tg.test_token_verified = true;
        tg.erc20_gas_cache.insert(Erc20Function::Transfer, 21_000);
        tg.guzzler_gas_cache
            .insert(GuzzlerFunction::HashLoop, 100_000);

        let json = serde_json::to_string(&tg).expect("serialise");
        let mut restored: TxGenerator = serde_json::from_str(&json).expect("deserialise");

        assert_eq!(restored.id, tg.id);
        assert_eq!(restored.signers_range, tg.signers_range);
        assert_eq!(restored.next_nonces, tg.next_nonces);
        assert_eq!(restored.tx_counts, tg.tx_counts);
        assert_eq!(restored.next_account_index, tg.next_account_index);
        assert_eq!(restored.skipped_accounts, tg.skipped_accounts);
        assert_eq!(restored.guzzler_verified, tg.guzzler_verified);
        assert_eq!(restored.test_token_verified, tg.test_token_verified);
        assert_eq!(restored.erc20_gas_cache, tg.erc20_gas_cache);
        assert_eq!(restored.guzzler_gas_cache, tg.guzzler_gas_cache);
        // Skipped fields default on deserialise — signers, channels, WS clients.
        assert!(restored.signers.is_empty());
        assert!(restored.tx_sender.is_none());
        assert!(restored.ws_client_builders.is_empty());
        assert!(restored.ws_clients.is_none());

        // ensure_signers_capacity resizes to one slot per account in range,
        // each holding None until next_tx lazily derives the BIP32 key.
        restored.ensure_signers_capacity();
        assert_eq!(restored.signers.len(), restored.signers_range.len());
        assert!(restored.signers.iter().all(Option::is_none));
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
            let (tx_sender, mut tx_receiver) =
                mpsc::channel::<(TxEnvelope, usize)>(channel_capacity);
            // The generator no longer acks optimistically — the sender drives
            // ack/refresh via a back-channel. The test only exercises tx
            // distribution, so wire up an ack receiver that's never written to.
            let (_ack_tx, ack_rx) = mpsc::channel::<AckOutcome>(channel_capacity);
            let mut generator = make_generator(start, end, Some(tx_sender), 0);

            // When we run the generator briefly to fill up the channel
            let handle = tokio::spawn(async move { generator.run(ack_rx).await });
            tokio::time::sleep(Duration::from_millis(channel_capacity as u64)).await;
            handle.abort(); // to stop producing more txs
            let _ = handle.await; // ignore join errors from abort

            // Drain generated txs from channel and count txs per signer (by recovered sender address)
            let mut per_sender_counts: HashMap<Address, usize> = HashMap::new();
            let mut counter = 0usize;
            while let Ok((envelope, _account_index)) = tx_receiver.try_recv() {
                let sender = envelope.recover_signer().expect("recover signer");
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
            tx1.nonce(),
            tx2.nonce(),
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
            tx2.nonce(),
            tx1.nonce() + 1,
            "nonce should increment after ack"
        );
        Ok(())
    }

    #[tokio::test]
    async fn next_tx_legacy_produces_legacy_envelope() -> Result<()> {
        let account_builder = AccountBuilder::new(TEST_MNEMONIC.to_string());
        let mut generator = TxGenerator::new(
            0,
            0..1,
            account_builder,
            vec![],
            None,
            0,
            false,
            0,
            GuzzlerFnWeights::default(),
            Erc20FnWeights::default(),
            TxTypeMix {
                legacy: 100,
                ..Default::default()
            },
        );

        let (envelope, _) = generator.next_tx().await?.expect("legacy tx");
        assert!(
            matches!(envelope, TxEnvelope::Legacy(_)),
            "expected Legacy envelope, got {:?}",
            envelope
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
