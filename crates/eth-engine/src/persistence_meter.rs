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

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_eips::BlockNumHash;
use async_trait::async_trait;
use eyre::{eyre, Context};
use jsonrpsee::{
    async_client::Client,
    core::client::SubscriptionClientT,
    rpc_params,
    ws_client::{PingConfig, WsClientBuilder},
};
use reth_ipc::client::IpcClientBuilder;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::engine::{EthereumAPI, SubscriptionEndpoint};

const SUBSCRIBE_METHOD: &str = "reth_subscribePersistedBlock";
const UNSUBSCRIBE_METHOD: &str = "reth_unsubscribePersistedBlock";
/// Timeout for the initial WebSocket/IPC connection attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Timeout for individual RPC requests on the subscription connection.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Initial delay between reconnection attempts (doubles on each failure, capped).
const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
/// WebSocket ping interval for detecting stale connections.
const WS_PING_INTERVAL: Duration = Duration::from_secs(30);
/// Cap on exponential reconnect backoff.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);

/// Tracks execution layer block persistence and applies backpressure when the
/// EL falls behind.
///
/// Callers invoke [`wait_for_persisted_block`] after submitting a block to the
/// EL. The call returns immediately if the EL has already persisted within the
/// configured threshold, or blocks until persistence catches up. Returns `Err`
/// on timeout.
#[cfg_attr(any(test, feature = "mocks"), mockall::automock)]
#[async_trait]
pub trait PersistenceMeter: Send + Sync {
    /// Block until `block_number` (minus the configured threshold) has been
    /// persisted by the execution layer. Returns immediately if already satisfied.
    /// Returns `Err` on timeout
    async fn wait_for_persisted_block(
        &self,
        block_number: u64,
        timeout: Duration,
    ) -> eyre::Result<()>;

    /// Set a known persisted block height so early wait calls don't block
    /// unnecessarily. Only reliable at startup before blocks accumulate.
    fn seed(&self, _block_number: u64) {}
}

#[async_trait]
impl<T: PersistenceMeter + ?Sized> PersistenceMeter for &T {
    async fn wait_for_persisted_block(
        &self,
        block_number: u64,
        timeout: Duration,
    ) -> eyre::Result<()> {
        (**self)
            .wait_for_persisted_block(block_number, timeout)
            .await
    }

    fn seed(&self, block_number: u64) {
        (**self).seed(block_number);
    }
}
pub struct NoopPersistenceMeter;

#[async_trait]
impl PersistenceMeter for NoopPersistenceMeter {
    async fn wait_for_persisted_block(
        &self,
        _block_number: u64,
        _timeout: Duration,
    ) -> eyre::Result<()> {
        Ok(())
    }
}

/// Statuses that the persistence meter internally transitions between
///
/// Subscription live and counter reliable. Backpressure enforced.
const SUBSCRIPTION_STATUS_ACTIVE: u8 = 0;
/// No live subscription. Backpressure suspended.
const SUBSCRIPTION_STATUS_RECONNECTING: u8 = 1;
/// Subscription live but no notification received yet. Backpressure suspended.
const SUBSCRIPTION_STATUS_CONNECTED: u8 = 2;

struct SharedState {
    /// Highest block number the EL has confirmed as persisted to disk.
    last_persisted_block: AtomicU64,
    /// Wakes waiters when `last_persisted_block` advances or connection state changes.
    notify: Notify,
    /// Subscription lifecycle state. Backpressure is only enforced when
    /// [`SUBSCRIPTION_STATUS_ACTIVE`].
    subscription_status: AtomicU8,
}

/// Meter block ingestion throughput using the execution layer's persisted block subscription.
///
/// A background task maintains the subscription connection, updates an atomic
/// counter on each notification, and wakes any waiters via [`Notify`]. This
/// allows [`wait_for_persisted_block`] to return immediately when the target
/// block is already persisted, and multiple callers can wait concurrently
/// without contending on a lock.
///
/// In the background, a task will maintain the connection, reconnecting as needed.
/// When reconnecting, the internal [`subscription_status`] will transition to [`SUBSCRIPTION_STATUS_RECONNECTING`]
/// which disables backpressure. Upon the first received notification, it will transition back to [`SUBSCRIPTION_STATUS_ACTIVE`],
/// applying backpressure again.
///
/// Seeding the meter with an initial height value will transition it to
/// [`SUBSCRIPTION_STATUS_ACTIVE`] only if the subscription is already
/// [`SUBSCRIPTION_STATUS_CONNECTED`]. If reconnecting, the seed updates the
/// counter but backpressure remains suspended until the subscription is live.
pub struct PersistedBlockMeter {
    shared: Arc<SharedState>,
    /// Number of blocks the EL may lag behind before backpressure applies.
    persistence_backpressure_threshold: u64,
    /// Background subscription reader; aborted on drop.
    _background: tokio::task::JoinHandle<()>,
}

impl PersistedBlockMeter {
    async fn new(endpoint: &SubscriptionEndpoint, persistence_backpressure_threshold: u64) -> Self {
        let initial_connection = connect_and_subscribe(endpoint).await.ok();
        let status = if initial_connection.is_some() {
            info!("Persistence meter: subscription connected");
            SUBSCRIPTION_STATUS_CONNECTED
        } else {
            warn!("Persistence meter: initial connection failed; retrying in background");
            SUBSCRIPTION_STATUS_RECONNECTING
        };

        let shared = Arc::new(SharedState {
            last_persisted_block: AtomicU64::new(0),
            notify: Notify::new(),
            subscription_status: AtomicU8::new(status),
        });

        let background = tokio::spawn(background_reader(
            endpoint.clone(),
            Arc::clone(&shared),
            initial_connection,
        ));

        Self {
            shared,
            persistence_backpressure_threshold,
            _background: background,
        }
    }
}

#[cfg(test)]
impl PersistedBlockMeter {
    fn test_instance(persistence_backpressure_threshold: u64) -> Self {
        let shared = Arc::new(SharedState {
            last_persisted_block: AtomicU64::new(0),
            notify: Notify::new(),
            subscription_status: AtomicU8::new(SUBSCRIPTION_STATUS_ACTIVE),
        });
        Self {
            shared,
            persistence_backpressure_threshold,
            _background: tokio::spawn(std::future::pending()),
        }
    }
}

impl Drop for PersistedBlockMeter {
    fn drop(&mut self) {
        self._background.abort();
    }
}

#[async_trait]
impl PersistenceMeter for PersistedBlockMeter {
    fn seed(&self, block_number: u64) {
        self.shared
            .last_persisted_block
            .fetch_max(block_number, Ordering::Release);

        // Only transition to ACTIVE if there's a functioning connection
        // This is to prevent the situation where the connection can never be
        // established, yet back pressure would still be applied.
        let activated = self
            .shared
            .subscription_status
            .compare_exchange(
                SUBSCRIPTION_STATUS_CONNECTED,
                SUBSCRIPTION_STATUS_ACTIVE,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok();
        self.shared.notify.notify_waiters();
        if activated {
            info!(
                persisted_block = block_number,
                "Persistence meter: seeded and active"
            );
        } else {
            info!(
                persisted_block = block_number,
                "Persistence meter: seeded (backpressure deferred until subscription is live)"
            );
        }
    }

    async fn wait_for_persisted_block(
        &self,
        block_number: u64,
        timeout: Duration,
    ) -> eyre::Result<()> {
        let target = block_number.saturating_sub(self.persistence_backpressure_threshold);
        let started_at = Instant::now();

        loop {
            // Register for notification *before* checking state to avoid
            // missing an update between the checks and the await.
            let notified = self.shared.notify.notified();

            // Only enforce backpressure when ACTIVE.
            let status = self.shared.subscription_status.load(Ordering::Acquire);
            if status != SUBSCRIPTION_STATUS_ACTIVE {
                debug!(
                    requested_block = block_number,
                    status, "Persistence backpressure skipped: subscription not active"
                );
                return Ok(());
            }

            let current = self.shared.last_persisted_block.load(Ordering::Acquire);
            // If we're within the configured threshold, proceed
            if current >= target {
                debug!(
                    requested_block = block_number,
                    persisted_block = current,
                    "Persistence backpressure satisfied: EL is keeping up"
                );
                return Ok(());
            }

            // Else, wait until timeout is reached
            let remaining = timeout
                .checked_sub(started_at.elapsed())
                .unwrap_or(Duration::ZERO);

            if remaining.is_zero() {
                return Err(eyre!(
                    "Persistence backpressure timed out: requested block {block_number}, \
                     persisted block {current}"
                ));
            }

            debug!(
                requested_block = block_number,
                persisted_block = current,
                "Persistence backpressure: waiting for execution layer to catch up"
            );

            if tokio::time::timeout(remaining, notified).await.is_err() {
                let current = self.shared.last_persisted_block.load(Ordering::Acquire);
                return Err(eyre!(
                    "Persistence backpressure timed out: requested block {block_number}, \
                     persisted block {current}"
                ));
            }
        }
    }
}

/// Background task that reads persisted block notifications from the execution
/// layer and updates the shared atomic counter. Reconnects automatically on error.
///
/// If `initial_connection` is `None`, the task begins by connecting via `reconnect()`.
async fn background_reader(
    endpoint: SubscriptionEndpoint,
    shared: Arc<SharedState>,
    initial_connection: Option<(Client, jsonrpsee::core::client::Subscription<BlockNumHash>)>,
) {
    let (mut client, mut subscription) = match initial_connection {
        Some(conn) => conn,
        None => reconnect(&endpoint).await,
    };

    loop {
        match subscription.next().await {
            Some(Ok(block)) => {
                // If this is the first notification received since re-establishing
                // a connection, transition to ACTIVE
                let prev_status = shared
                    .subscription_status
                    .swap(SUBSCRIPTION_STATUS_ACTIVE, Ordering::Release);
                if prev_status != SUBSCRIPTION_STATUS_ACTIVE {
                    info!(
                        persisted_block = block.number,
                        "Persistence meter: subscription now active"
                    );
                }
                let prev = shared
                    .last_persisted_block
                    .fetch_max(block.number, Ordering::Release);
                // Only notify if we've advanced, in case they arrive out-of-order
                if block.number > prev {
                    shared.notify.notify_waiters();
                }
                debug!(
                    persisted_block = block.number,
                    "Persistence meter: received persisted block notification"
                );
            }
            Some(Err(error)) => {
                warn!(
                    %error,
                    "Persistence meter: subscription errored; reconnecting"
                );
                shared
                    .subscription_status
                    .store(SUBSCRIPTION_STATUS_RECONNECTING, Ordering::Release);
                shared.notify.notify_waiters();
                drop(subscription);
                drop(client);
                (client, subscription) = reconnect(&endpoint).await;
            }
            None => {
                warn!("Persistence meter: subscription closed; reconnecting");
                shared
                    .subscription_status
                    .store(SUBSCRIPTION_STATUS_RECONNECTING, Ordering::Release);
                shared.notify.notify_waiters();
                drop(subscription);
                drop(client);
                (client, subscription) = reconnect(&endpoint).await;
            }
        }
    }
}

/// Retry the subscription connection with exponential backoff (1s → 60s cap).
///
/// Retries forever. This runs in the background task, not in the caller's
/// loop, so it never directly blocks block processing.
async fn reconnect(
    endpoint: &SubscriptionEndpoint,
) -> (Client, jsonrpsee::core::client::Subscription<BlockNumHash>) {
    let mut backoff = RECONNECT_BACKOFF;
    loop {
        match connect_and_subscribe(endpoint).await {
            Ok((client, subscription)) => {
                info!("Persistence meter: reconnected to persisted block subscription");
                return (client, subscription);
            }
            Err(error) => {
                warn!(
                    %error,
                    backoff = ?backoff,
                    "Persistence meter: reconnection failed; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_RECONNECT_BACKOFF);
            }
        }
    }
}

/// Establish a fresh connection and subscribe to persisted block
/// notifications. Used both for the initial connection in
/// [`PersistedBlockMeter::new`] and for reconnection attempts in
/// [`reconnect`].
async fn connect_and_subscribe(
    endpoint: &SubscriptionEndpoint,
) -> eyre::Result<(Client, jsonrpsee::core::client::Subscription<BlockNumHash>)> {
    let client = connect_client(endpoint).await?;
    let subscription = client
        .subscribe(SUBSCRIBE_METHOD, rpc_params![], UNSUBSCRIBE_METHOD)
        .await
        .wrap_err("Failed to subscribe to persisted block notifications")?;

    Ok((client, subscription))
}

/// Open a JSON-RPC client to the execution layer.
///
/// Dispatches on [`SubscriptionEndpoint`]: IPC connects to a Unix socket,
/// WS connects over WebSocket with 'keep-alive' ping.
async fn connect_client(endpoint: &SubscriptionEndpoint) -> eyre::Result<Client> {
    match endpoint {
        SubscriptionEndpoint::Ipc { socket_path } => IpcClientBuilder::default()
            .request_timeout(REQUEST_TIMEOUT)
            .build(socket_path)
            .await
            .wrap_err_with(|| format!("Failed to connect to IPC socket {socket_path}")),
        SubscriptionEndpoint::Ws { url } => WsClientBuilder::default()
            .request_timeout(REQUEST_TIMEOUT)
            .connection_timeout(CONNECT_TIMEOUT)
            .enable_ws_ping(
                PingConfig::new()
                    .ping_interval(WS_PING_INTERVAL)
                    .inactive_limit(WS_PING_INTERVAL * 2),
            )
            .build(url)
            .await
            .wrap_err_with(|| format!("Failed to connect to WebSocket endpoint {url}")),
    }
}

/// Create a persistence meter. The initial connection is attempted eagerly;
/// if it fails, the background task retries automatically.
///
/// The meter starts without a known persisted height. Call
/// [`PersistenceMeter::seed`] after creation if a reliable starting
/// height is available (e.g. at startup via `eth_getBlockByNumber`).
pub async fn create(
    endpoint: &SubscriptionEndpoint,
    persistence_backpressure_threshold: u64,
) -> Box<dyn PersistenceMeter> {
    let m = PersistedBlockMeter::new(endpoint, persistence_backpressure_threshold).await;
    info!(
        backpressure_threshold = persistence_backpressure_threshold,
        "Persistence meter: active"
    );
    Box::new(m)
}

/// Create a persistence meter with graceful degradation. Falls back to a no-op
/// meter (no backpressure) when:
/// - `enabled` is false
/// - no subscription endpoint is configured
///
/// The meter starts without a known persisted height. Call
/// [`PersistenceMeter::seed`] after creation if a reliable starting
/// height is available (e.g. at startup via `eth_getBlockByNumber`).
pub async fn create_with_fallback(
    enabled: bool,
    endpoint: Option<&SubscriptionEndpoint>,
    persistence_backpressure_threshold: u64,
) -> Box<dyn PersistenceMeter> {
    if !enabled {
        info!("Persistence meter: disabled, --execution-persistence-backpressure not set");
        return Box::new(NoopPersistenceMeter);
    }
    let Some(endpoint) = endpoint else {
        info!("Persistence meter: disabled, no subscription endpoint configured");
        return Box::new(NoopPersistenceMeter);
    };
    create(endpoint, persistence_backpressure_threshold).await
}

/// Seed the meter with the EL's current block height. Only reliable at
/// startup before blocks accumulate, since `eth_getBlockByNumber("latest")`
/// may return un-persisted heights during steady-state operation.
pub async fn seed_from_latest_block(meter: &dyn PersistenceMeter, eth: &dyn EthereumAPI) {
    match eth.get_block_by_number("latest").await {
        Ok(Some(block)) => {
            meter.seed(block.block_number);
        }
        Ok(None) => {
            warn!("Persistence meter: seed skipped: Ethereum API returned no latest block");
        }
        Err(error) => {
            warn!(
                %error,
                "Persistence meter: seed skipped: failed to fetch latest Ethereum block"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MockEthereumAPI;
    use eyre::eyre;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);
    const SHORT_TIMEOUT: Duration = Duration::from_millis(50);
    use reqwest::Url;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn noop_meter_always_succeeds() {
        let meter = NoopPersistenceMeter;
        assert!(meter
            .wait_for_persisted_block(100, TEST_TIMEOUT)
            .await
            .is_ok());
        assert!(meter
            .wait_for_persisted_block(0, TEST_TIMEOUT)
            .await
            .is_ok());
        assert!(meter
            .wait_for_persisted_block(u64::MAX, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn create_with_fallback_returns_noop_when_disabled() {
        let endpoint = SubscriptionEndpoint::Ws {
            url: Url::parse("ws://localhost:8546").unwrap(),
        };
        let meter = create_with_fallback(false, Some(&endpoint), 100).await;
        assert!(meter
            .wait_for_persisted_block(1000, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn create_with_fallback_returns_noop_when_no_endpoint() {
        let meter = create_with_fallback(true, None, 100).await;
        assert!(meter
            .wait_for_persisted_block(1000, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn create_with_fallback_suspends_backpressure_on_connection_failure() {
        let endpoint = SubscriptionEndpoint::Ws {
            url: Url::parse("ws://192.0.2.1:1").unwrap(), // unreachable
        };
        let meter = create_with_fallback(true, Some(&endpoint), 5).await;
        assert!(meter
            .wait_for_persisted_block(1000, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn create_retries_on_unreachable_endpoint() {
        let endpoint = SubscriptionEndpoint::Ws {
            url: Url::parse("ws://192.0.2.1:1").unwrap(),
        };
        let meter = create(&endpoint, 5).await;
        // Initial connection failed — backpressure suspended (not ACTIVE)
        assert!(meter
            .wait_for_persisted_block(1000, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_already_persisted() {
        let meter = PersistedBlockMeter::test_instance(5);
        meter
            .shared
            .last_persisted_block
            .store(100, Ordering::Release);

        // block_number=10, threshold=5, target=5 — persisted=100 >= 5
        assert!(meter
            .wait_for_persisted_block(10, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn wait_returns_immediately_when_threshold_covers_gap() {
        let meter = PersistedBlockMeter::test_instance(10);
        meter
            .shared
            .last_persisted_block
            .store(0, Ordering::Release);

        // block_number=5, threshold=10, target=5.saturating_sub(10)=0 — persisted=0 >= 0
        assert!(meter
            .wait_for_persisted_block(5, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn wait_times_out_when_not_persisted() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .last_persisted_block
            .store(5, Ordering::Release);

        // target=100, persisted=5 — will never catch up
        let result = meter.wait_for_persisted_block(100, SHORT_TIMEOUT).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn wait_skips_backpressure_when_reconnecting() {
        let meter = PersistedBlockMeter::test_instance(0);
        // persisted=0, target=100 — would normally block and time out
        meter
            .shared
            .subscription_status
            .store(SUBSCRIPTION_STATUS_RECONNECTING, Ordering::Release);
        assert!(meter
            .wait_for_persisted_block(100, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn wait_enforces_backpressure_when_active() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .last_persisted_block
            .store(5, Ordering::Release);
        // Status is ACTIVE (default), persisted=5, target=100 — should block and time out
        let result = meter.wait_for_persisted_block(100, SHORT_TIMEOUT).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn seed_transitions_connected_to_active() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .subscription_status
            .store(SUBSCRIPTION_STATUS_CONNECTED, Ordering::Release);
        meter.seed(100);
        assert_eq!(
            meter.shared.subscription_status.load(Ordering::Acquire),
            SUBSCRIPTION_STATUS_ACTIVE
        );
    }

    #[tokio::test]
    async fn seed_does_not_activate_when_reconnecting() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .subscription_status
            .store(SUBSCRIPTION_STATUS_RECONNECTING, Ordering::Release);
        meter.seed(100);
        assert_eq!(
            meter.shared.subscription_status.load(Ordering::Acquire),
            SUBSCRIPTION_STATUS_RECONNECTING
        );
        assert_eq!(
            meter.shared.last_persisted_block.load(Ordering::Acquire),
            100
        );
    }

    #[tokio::test]
    async fn wait_skips_backpressure_when_connected_but_not_active() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .subscription_status
            .store(SUBSCRIPTION_STATUS_CONNECTED, Ordering::Release);
        assert!(meter
            .wait_for_persisted_block(100, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn wait_wakes_up_on_notify() {
        let meter = PersistedBlockMeter::test_instance(0);
        meter
            .shared
            .last_persisted_block
            .store(0, Ordering::Release);

        let shared = Arc::clone(&meter.shared);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            shared.last_persisted_block.store(50, Ordering::Release);
            shared.notify.notify_waiters();
        });

        assert!(meter
            .wait_for_persisted_block(50, TEST_TIMEOUT)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn seed_updates_persisted_block() {
        let meter = PersistedBlockMeter::test_instance(5);
        assert_eq!(meter.shared.last_persisted_block.load(Ordering::Acquire), 0);

        meter.seed(42);
        assert_eq!(
            meter.shared.last_persisted_block.load(Ordering::Acquire),
            42
        );

        // seed should only increase, never decrease
        meter.seed(10);
        assert_eq!(
            meter.shared.last_persisted_block.load(Ordering::Acquire),
            42
        );
    }

    #[tokio::test]
    async fn seed_from_latest_block_updates_meter() {
        let meter = PersistedBlockMeter::test_instance(5);

        let mut eth = MockEthereumAPI::new();
        eth.expect_get_block_by_number().return_once(|_| {
            Ok(Some(crate::json_structures::ExecutionBlock {
                block_number: 99,
                block_hash: Default::default(),
                parent_hash: Default::default(),
                timestamp: 0,
            }))
        });

        seed_from_latest_block(&meter, &eth).await;
        assert_eq!(
            meter.shared.last_persisted_block.load(Ordering::Acquire),
            99
        );
    }

    #[tokio::test]
    async fn seed_from_latest_block_handles_no_block() {
        let meter = PersistedBlockMeter::test_instance(5);

        let mut eth = MockEthereumAPI::new();
        eth.expect_get_block_by_number().return_once(|_| Ok(None));

        seed_from_latest_block(&meter, &eth).await;
        assert_eq!(meter.shared.last_persisted_block.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn seed_from_latest_block_handles_error() {
        let meter = PersistedBlockMeter::test_instance(5);

        let mut eth = MockEthereumAPI::new();
        eth.expect_get_block_by_number()
            .return_once(|_| Err(eyre!("connection refused")));

        seed_from_latest_block(&meter, &eth).await;
        assert_eq!(meter.shared.last_persisted_block.load(Ordering::Acquire), 0);
    }
}
