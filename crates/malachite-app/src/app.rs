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

use std::future::Future;
use std::sync::Arc;

use eyre::{eyre, Context as _};
use tokio::sync::mpsc::Receiver;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use malachitebft_app_channel::{AppMsg, Channels};
use malachitebft_core_types::utils::height::DisplayRange;

use arc_consensus_db::invalid_payloads::StoredInvalidPayloads;
use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::proposal_monitor::ProposalMonitor;
use arc_consensus_types::proposer::{ProposerSelector, RoundRobin};
use arc_consensus_types::{ArcContext, Height, StoredCommitCertificate};
use arc_eth_engine::engine::Engine;

use crate::handlers::*;
use crate::metrics::AppMetrics;
use crate::request::{
    health_from_heartbeat, AppRequest, CommitCertificateInfo, Health, HeightRangeRequest,
    InvalidPayloadsRequest, Status,
};
use crate::state::{State, StatusSnapshot};
use crate::stats::Stats;
use crate::store::{
    HeightRangePage, RangeFailureReason, RangeQueryResult, Store, MAX_RANGE_QUERY_BYTES,
};
use crate::utils::sync_state::SyncState;

/// Returned by [`go`] when the consensus channel closes.
///
/// During a graceful shutdown the Node actor is stopped first, which closes this
/// channel on purpose; outside of shutdown it means consensus stopped unexpectedly
/// and the node should restart.
#[derive(Debug, thiserror::Error)]
#[error("Consensus channel closed unexpectedly")]
struct ConsensusChannelClosed;

pub async fn run(
    mut state: State,
    channels: Channels<ArcContext>,
    engine: Engine,
    rx_app_req: Receiver<AppRequest>,
    cancel_token: CancellationToken,
    graceful_shutdown: CancellationToken,
) -> eyre::Result<()> {
    if let Some(halt_height) = state.env_config().halt_height {
        warn!("Consensus configured to halt at block height: {halt_height}");
    }

    let (status_tx, status_rx) = watch::channel(state.status_snapshot());
    // `None` until `ConsensusReady` completes; see `go`.
    let (heartbeat_tx, heartbeat_rx) = watch::channel(None);

    let ctx = AppRequestContext {
        store: state.store().clone(),
        engine: engine.clone(),
        stats: state.stats().clone(),
        metrics: state.metrics().clone(),
        proposer_selector: state.ctx.proposer_selector,
        status_rx,
        heartbeat_rx,
    };

    let mut app_req_task: JoinHandle<eyre::Result<Never>> =
        tokio::spawn(process_app_requests(rx_app_req, ctx));

    let result = cancel_token
        .run_until_cancelled_owned(go(
            &mut state,
            channels,
            &engine,
            status_tx,
            heartbeat_tx,
            &mut app_req_task,
        ))
        .await;

    // Always abort the app-request task, including on graceful cancellation
    // where `go` is dropped at an await point and would otherwise detach it.
    app_req_task.abort();

    let result = match result {
        Some(Ok(never)) => match never {},
        Some(Err(e)) => {
            // A closed consensus channel during a graceful shutdown means the Node
            // actor was stopped first on purpose, so treat it as a clean exit. Any
            // other error — or a closed channel outside of shutdown — is a genuine
            // failure and must still surface so the node restarts.
            if graceful_shutdown.is_cancelled()
                && e.downcast_ref::<ConsensusChannelClosed>().is_some()
            {
                info!("🟢🟢 Application is shutting down gracefully");
                Ok(())
            } else {
                error!("🔴 Error in application: {e:#}");
                error!("🔴 Shutting down");
                Err(e)
            }
        }
        None => {
            info!("🟢🟢 Application is shutting down gracefully");
            Ok(())
        }
    };

    // Create a savepoint in the database helps avoiding its repair on next startup.
    state.savepoint();

    result
}

/// A type that cannot be instantiated.
///
/// Used to indicate that the function never returns normally.
enum Never {}

/// Context shared with the spawned app-request processing task.
///
/// All fields are cheap to clone (`Arc`-based or `Copy`).
#[derive(Clone)]
struct AppRequestContext {
    store: Store,
    engine: Engine,
    stats: Stats,
    metrics: AppMetrics,
    proposer_selector: RoundRobin,
    status_rx: watch::Receiver<StatusSnapshot>,
    /// Last time the consensus `go()` loop was able to poll; `None` until
    /// `ConsensusReady` completes.
    heartbeat_rx: watch::Receiver<Option<Instant>>,
}

/// Cap concurrent store/EL-backed app requests. Probes never take a slot.
///
/// The RPC channel can hold 64 messages; without a bound, a burst of range
/// queries would all hit a degraded EL at once.
const MAX_CONCURRENT_STORE_REQUESTS: usize = 4;

/// Runs in a dedicated tokio task, processing app requests concurrently
/// with the consensus event loop.
///
/// Liveness probes (`GetHealth`, `GetSyncState`) run inline so a slow
/// range query cannot stall `/health` or `/ready`. Store-backed requests
/// run on spawned tasks, bounded by [`MAX_CONCURRENT_STORE_REQUESTS`].
async fn process_app_requests(
    rx: Receiver<AppRequest>,
    ctx: AppRequestContext,
) -> eyre::Result<Never> {
    let metrics = ctx.metrics.clone();
    dispatch_app_requests(
        rx,
        move |req| {
            let ctx = ctx.clone();
            async move { handle_app_request(req, &ctx).await }
        },
        metrics,
    )
    .await
}

/// Probe-vs-spawn dispatch: probes run inline; other requests spawn, wait for
/// a store-request slot, then run `handle`. A caller that drops its reply
/// while the task is waiting for a slot loses that race and does no store or
/// execution-layer work. A panic in a spawned
/// task fails this future so `go()` still sees it through `app_req_task`.
async fn dispatch_app_requests<H, Fut>(
    mut rx: Receiver<AppRequest>,
    handle: H,
    metrics: AppMetrics,
) -> eyre::Result<Never>
where
    H: Fn(AppRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = eyre::Result<()>> + Send + 'static,
{
    let mut in_flight = JoinSet::new();
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT_STORE_REQUESTS));

    loop {
        tokio::select! {
            biased;

            Some(join) = in_flight.join_next(), if !in_flight.is_empty() => {
                if let Err(e) = join {
                    if e.is_cancelled() {
                        continue;
                    }
                    return Err(eyre!("App request handler panicked: {e}"));
                }
            }

            recv = rx.recv() => match recv {
                Some(mut req) => {
                    if req.is_liveness_probe() {
                        if let Err(e) = handle(req).await {
                            error!("🔴 Error handling application request: {e:#}");
                        }
                    } else {
                        let handle = handle.clone();
                        let slots = slots.clone();
                        let metrics = metrics.clone();
                        in_flight.spawn(async move {
                            // A task parked on the semaphore must leave when the
                            // caller has already timed out, instead of taking a
                            // slot later and running the handler anyway.
                            let Some(_permit) = acquire_store_slot(&mut req, slots).await else {
                                metrics.inc_app_request_skipped_count(req.metric_label());
                                return;
                            };
                            if let Err(e) = handle(req).await {
                                error!("🔴 Error handling application request: {e:#}");
                            }
                        });
                    }
                }
                None => {
                    return Err(eyre!("Application request channel closed unexpectedly"));
                }
            }
        }
    }
}

/// Wait for a store-request slot, or return `None` if the caller drops the
/// reply first. `biased` prefers the closed reply when both are ready, so a
/// request that is already abandoned does not take a slot that just opened.
async fn acquire_store_slot(
    req: &mut AppRequest,
    slots: Arc<Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    tokio::select! {
        biased;
        () = req.reply_closed() => None,
        permit = slots.acquire_owned() => {
            Some(permit.expect("app-request semaphore is never closed"))
        }
    }
}

/// The main event loop of the application.
///
/// It listens for messages from consensus and monitors the app-request task.
/// App requests are processed in a separate task so they don't block consensus.
///
/// # Errors
/// Returns an error if handling a message fails or one of the channels is closed unexpectedly.
/// This will cause the application to shut down.
/// Otherwise, it runs indefinitely until cancelled.
async fn go(
    state: &mut State,
    mut channels: Channels<ArcContext>,
    engine: &Engine,
    status_tx: watch::Sender<StatusSnapshot>,
    heartbeat_tx: watch::Sender<Option<Instant>>,
    app_req_task: &mut JoinHandle<eyre::Result<Never>>,
) -> eyre::Result<Never> {
    // Ticks while `go()` is polling. A tick does not fire while
    // `handle_consensus` is awaiting, so `/health` can detect a wedged loop.
    //
    // `ConsensusReady` runs handshake, replay and checkpoint sync inline on this
    // loop and can legitimately block it for far longer than the liveness budget,
    // so no heartbeat is published until it completes and `/health` stays OK.
    let mut heartbeat = interval(std::time::Duration::from_secs(1));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut startup_done = false;

    loop {
        tokio::select! {
            biased;

            msg = channels.consensus.recv() => match msg {
                Some(msg) => {
                    let is_consensus_ready = matches!(msg, AppMsg::ConsensusReady { .. });

                    // Abort on error to shut down the application.
                    handle_consensus(msg, state, &mut channels, engine).await
                        .wrap_err("Error handling consensus message")?;

                    // Skip the publish when nothing changed: most consensus messages
                    // (sync queries, restream requests, vote-extension hooks) leave the
                    // snapshot untouched, so there is no point updating the watch.
                    status_tx.send_if_modified(|current| {
                        let new = state.status_snapshot();
                        if *current != new {
                            *current = new;
                            true
                        } else {
                            false
                        }
                    });

                    startup_done |= is_consensus_ready;
                    if startup_done {
                        let _ = heartbeat_tx.send(Some(Instant::now()));
                    }
                },
                None => {
                    return Err(ConsensusChannelClosed.into());
                }
            },

            result = &mut *app_req_task => {
                // The app-request task should run forever; if it exits, propagate the error.
                match result {
                    Ok(Ok(never)) => match never {},
                    Ok(Err(e)) => return Err(e.wrap_err("App request task failed")),
                    Err(e) => return Err(eyre!("App request task panicked: {e}")),
                }
            },

            _ = heartbeat.tick(), if startup_done => {
                let _ = heartbeat_tx.send(Some(Instant::now()));
            },
        }
    }
}

async fn handle_consensus(
    msg: AppMsg<ArcContext>,
    state: &mut State,
    channels: &mut Channels<ArcContext>,
    engine: &Engine,
) -> eyre::Result<()> {
    match msg {
        // Consensus is ready.
        // The application replies with a message to instruct
        // consensus to start at a given height.
        AppMsg::ConsensusReady { reply } => {
            let _guard = state.metrics.start_msg_process_timer("ConsensusReady");

            info!("🚦 Consensus is ready");

            consensus_ready::handle(state, engine, reply).await?;
        }

        // Consensus has started a new round.
        // The application replies to this message with the previously undecided proposals for the round.
        AppMsg::StartedRound {
            height,
            round,
            proposer,
            role,
            reply_value,
        } => {
            let _guard = state.metrics.start_msg_process_timer("StartedRound");

            started_round::handle(state, engine, height, round, proposer, role, reply_value)
                .await?;
        }

        // Request to build a local value to propose.
        // The application replies to this message with the requested value within the timeout.
        AppMsg::GetValue {
            height,
            round,
            timeout,
            reply,
        } => {
            let _guard = state.metrics.start_msg_process_timer("GetValue");

            info!(%height, %round, "Consensus is requesting a value to propose");

            get_value::handle(
                state,
                channels.network.clone(),
                engine,
                height,
                round,
                timeout,
                reply,
            )
            .await?;
        }

        // Notification for a new proposal part.
        // If a full proposal can be assembled, the application responds
        // with the complete proposed value. Otherwise, it responds with `None`.
        AppMsg::ReceivedProposalPart {
            from,
            published_by,
            part,
            reply,
        } => {
            let _guard = state
                .metrics
                .start_msg_process_timer("ReceivedProposalPart");

            received_proposal_part::handle(state, engine, from, published_by, part, reply).await;
        }

        // Notification that consensus has decided a value.
        //
        // The reply acknowledges that the certificate has been durably stored so the sync
        // actor can advertise the new tip height. Without it, peers never learn we advanced
        // beyond startup and late joiners cannot value-sync from us. `decided::handle`
        // consumes the channel at the durability point.
        AppMsg::Decided {
            certificate,
            extensions: _,
            reply,
        } => {
            let _guard = state.metrics.start_msg_process_timer("Decided");

            let height = certificate.height;
            let round = certificate.round;
            let value_id = certificate.value_id;
            let signatures = certificate.commit_signatures.len();

            info!(%height, %round, %value_id, %signatures, "🎉 Consensus has decided on value");

            decided::handle(state, engine, certificate, reply).await?;
        }

        // Notification that a height has been finalized.
        AppMsg::Finalized {
            certificate,
            extensions: _,
            evidence,
            reply,
        } => {
            let _guard = state.metrics.start_msg_process_timer("Finalized");

            let height = certificate.height;
            let round = certificate.round;
            let value_id = certificate.value_id;
            let signatures = certificate.commit_signatures.len();

            info!(
                %height, %round, %value_id, %signatures,
                "📜 Consensus has finalized the height"
            );

            finalized::handle(state, certificate, evidence, reply).await?;
        }

        // A value has been synced from the network.
        // This may happen when the node is catching up with the network.
        AppMsg::ProcessSyncedValue {
            height,
            round,
            proposer,
            value_bytes,
            reply,
        } => {
            let _guard = state.metrics.start_msg_process_timer("ProcessSyncedValue");

            info!(%height, %round, "Processing synced value");

            process_synced_value::handle(
                state,
                engine,
                height,
                round,
                proposer,
                value_bytes,
                reply,
            )
            .await?;
        }

        // Request for previously decided blocks from the application's storage.
        AppMsg::GetDecidedValues { range, reply } => {
            info!(range = %DisplayRange(&range), "Received sync request");

            get_decided_values::handle(state, engine, range, reply).await?;
        }

        // Request for the earliest height available in the block store.
        AppMsg::GetHistoryMinHeight { reply } => {
            let _guard = state.metrics.start_msg_process_timer("GetHistoryMinHeight");

            get_history_min_height::handle(state, engine, reply).await?;
        }

        // Request to re-stream a proposal that was previously seen at valid_round or round (if valid_round is Nil).
        AppMsg::RestreamProposal {
            height,
            round,
            valid_round,
            address: _,
            value_id,
        } => {
            let _guard = state.metrics.start_msg_process_timer("RestreamProposal");

            info!(%height, %round, %valid_round, %value_id, "Restreaming proposal");

            restream_proposal::handle(state, channels, height, round, valid_round, value_id)
                .await?;
        }

        // Currently not supported
        // Request to extend a precommit
        AppMsg::ExtendVote { reply, .. } => {
            if let Err(e) = reply.send(None) {
                error!("🔴 Failed to send ExtendVote reply: {e:?}");
            }
        }

        // Currently not supported
        // Request to verify vote extensions
        AppMsg::VerifyVoteExtensions {
            extensions, reply, ..
        } => {
            let verdicts = extensions
                .into_iter()
                .map(|(address, _)| (address, Ok(())))
                .collect();
            if let Err(e) = reply.send(verdicts) {
                error!("🔴 Failed to send VerifyVoteExtensions reply: {e:?}");
            }
        }
    }

    Ok(())
}

async fn handle_app_request(req: AppRequest, ctx: &AppRequestContext) -> eyre::Result<()> {
    let label = req.metric_label();
    if req.reply_is_closed() {
        ctx.metrics.inc_app_request_skipped_count(label);
        return Ok(());
    }
    ctx.metrics
        .observe_app_request_queue_time(label, req.enqueued_at().elapsed().as_secs_f64());
    let _guard = ctx.metrics.start_app_request_process_timer(label);

    match req {
        AppRequest::GetCertificate { height, reply, .. } => {
            handle_get_certificate(ctx, height, reply).await
        }
        AppRequest::GetMisbehaviorEvidence { height, reply, .. } => {
            handle_get_misbehavior_evidence(ctx, height, reply).await
        }
        AppRequest::GetProposalMonitorData { height, reply, .. } => {
            handle_get_proposal_monitor_data(ctx, height, reply).await
        }
        AppRequest::GetInvalidPayloads { height, reply, .. } => {
            handle_get_invalid_payloads(ctx, height, reply).await
        }
        AppRequest::GetCertificateRange { range, reply, .. } => {
            handle_get_certificate_range(ctx, range, reply).await
        }
        AppRequest::GetMisbehaviorEvidenceRange { range, reply, .. } => {
            handle_get_misbehavior_evidence_range(ctx, range, reply).await
        }
        AppRequest::GetProposalMonitorDataRange { range, reply, .. } => {
            handle_get_proposal_monitor_data_range(ctx, range, reply).await
        }
        AppRequest::GetInvalidPayloadsRange { request, reply, .. } => {
            handle_get_invalid_payloads_range(ctx, request, reply).await
        }
        AppRequest::GetStatus { reply, .. } => handle_get_status(ctx, reply).await,
        AppRequest::GetHealth { reply, .. } => {
            handle_get_health(ctx, reply);
            Ok(())
        }
        AppRequest::GetSyncState { reply, .. } => {
            handle_get_sync_state(ctx, reply);
            Ok(())
        }
    }
}

fn reply_or_log<T: std::fmt::Debug>(reply: oneshot::Sender<T>, value: T, label: &str) {
    if let Err(e) = reply.send(value) {
        error!("{label}: Failed to reply: {e:?}");
    }
}

async fn handle_get_certificate(
    ctx: &AppRequestContext,
    height: Option<Height>,
    reply: oneshot::Sender<Option<CommitCertificateInfo>>,
) -> eyre::Result<()> {
    let result = ctx.store.get_certificate(height).await.wrap_err_with(|| {
        format!("GetCertificate: Failed to get certificate for height {height:?}")
    })?;

    let info = match result {
        Some(certificate) => {
            get_certificate_info(
                ctx.proposer_selector,
                &ctx.engine,
                &ctx.metrics,
                certificate,
            )
            .await
        }
        None => None,
    };

    reply_or_log(reply, info, "GetCertificate");
    Ok(())
}

async fn handle_get_misbehavior_evidence(
    ctx: &AppRequestContext,
    height: Option<Height>,
    reply: oneshot::Sender<Option<StoredMisbehaviorEvidence>>,
) -> eyre::Result<()> {
    let evidence = ctx
        .store
        .get_misbehavior_evidence(height)
        .await
        .wrap_err_with(|| {
            format!(
                "GetMisbehaviorEvidence: Failed to get misbehavior evidence for height {height:?}",
            )
        })?;
    reply_or_log(reply, evidence, "GetMisbehaviorEvidence");
    Ok(())
}

async fn handle_get_proposal_monitor_data(
    ctx: &AppRequestContext,
    height: Option<Height>,
    reply: oneshot::Sender<Option<ProposalMonitor>>,
) -> eyre::Result<()> {
    let data = ctx
        .store
        .get_proposal_monitor_data(height)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to get proposal monitor data for height {:?} in response to a GetProposalMonitorData request",
                height,
            )
        })?;
    reply_or_log(reply, data, "GetProposalMonitorData");
    Ok(())
}

async fn handle_get_invalid_payloads(
    ctx: &AppRequestContext,
    height: Option<Height>,
    reply: oneshot::Sender<Option<StoredInvalidPayloads>>,
) -> eyre::Result<()> {
    let payloads = ctx.store.get_invalid_payloads(height).await.wrap_err_with(|| {
        format!(
            "Failed to get invalid payloads for height {:?} in response to a GetInvalidPayloads request",
            height,
        )
    })?;
    reply_or_log(reply, payloads, "GetInvalidPayloads");
    Ok(())
}

async fn handle_get_certificate_range(
    ctx: &AppRequestContext,
    range: HeightRangeRequest,
    reply: oneshot::Sender<Option<HeightRangePage<CommitCertificateInfo>>>,
) -> eyre::Result<()> {
    let info = certificate_range_page(range, ctx, || reply.is_closed()).await?;
    if reply.is_closed() {
        return Ok(());
    }
    reply_or_log(reply, info, "GetCertificateRange");
    Ok(())
}

async fn handle_get_misbehavior_evidence_range(
    ctx: &AppRequestContext,
    range: HeightRangeRequest,
    reply: oneshot::Sender<Option<HeightRangePage<StoredMisbehaviorEvidence>>>,
) -> eyre::Result<()> {
    let evidence = ctx
        .store
        .get_misbehavior_evidence_range(range.from, range.count, MAX_RANGE_QUERY_BYTES)
        .await
        .wrap_err_with(|| {
            format!("GetMisbehaviorEvidenceRange: Failed to get misbehavior evidence for {range:?}")
        })?;
    reply_or_log(reply, evidence, "GetMisbehaviorEvidenceRange");
    Ok(())
}

async fn handle_get_proposal_monitor_data_range(
    ctx: &AppRequestContext,
    range: HeightRangeRequest,
    reply: oneshot::Sender<Option<HeightRangePage<ProposalMonitor>>>,
) -> eyre::Result<()> {
    let data = ctx
        .store
        .get_proposal_monitor_data_range(range.from, range.count, MAX_RANGE_QUERY_BYTES)
        .await
        .wrap_err_with(|| {
            format!(
                "GetProposalMonitorDataRange: Failed to get proposal monitor data for {range:?}"
            )
        })?;
    reply_or_log(reply, data, "GetProposalMonitorDataRange");
    Ok(())
}

async fn handle_get_invalid_payloads_range(
    ctx: &AppRequestContext,
    request: InvalidPayloadsRequest,
    reply: oneshot::Sender<Option<HeightRangePage<StoredInvalidPayloads>>>,
) -> eyre::Result<()> {
    let payloads = ctx
        .store
        .get_invalid_payloads_range(request.from, request.count, MAX_RANGE_QUERY_BYTES)
        .await
        .wrap_err_with(|| {
            format!("GetInvalidPayloadsRange: Failed to get invalid payloads for {request:?}")
        })?;
    reply_or_log(reply, payloads, "GetInvalidPayloadsRange");
    Ok(())
}

async fn handle_get_status(
    ctx: &AppRequestContext,
    reply: oneshot::Sender<Status>,
) -> eyre::Result<()> {
    let snapshot = ctx.status_rx.borrow().clone();
    let status = snapshot
        .get_status(&ctx.store, &ctx.stats)
        .await
        .wrap_err("GetStatus: Failed to get the current status")?;

    reply_or_log(reply, status, "GetStatus");
    Ok(())
}

fn handle_get_health(ctx: &AppRequestContext, reply: oneshot::Sender<Health>) {
    reply_or_log(reply, health_from_heartbeat(&ctx.heartbeat_rx), "GetHealth");
}

fn handle_get_sync_state(ctx: &AppRequestContext, reply: oneshot::Sender<SyncState>) {
    let sync_state = ctx.status_rx.borrow().sync_state;
    reply_or_log(reply, sync_state, "GetSyncState");
}

async fn get_certificate_info(
    proposer_selector: impl ProposerSelector,
    engine: &Engine,
    metrics: &AppMetrics,
    stored: StoredCommitCertificate,
) -> Option<CommitCertificateInfo> {
    if let Some(proposer) = stored.proposer {
        return Some(CommitCertificateInfo {
            certificate: stored.certificate,
            certificate_type: stored.certificate_type,
            proposer,
        });
    }

    let validator_set = {
        let _guard =
            metrics.start_engine_api_timer("get_certificate_info.get_signing_validator_set");
        engine
            .eth
            .get_signing_validator_set(stored.certificate.height.as_u64())
            .await
            .ok()?
    };

    let proposer = proposer_selector
        .select_proposer(
            &validator_set,
            stored.certificate.height,
            stored.certificate.round,
        )
        .address;

    Some(CommitCertificateInfo {
        certificate: stored.certificate,
        certificate_type: stored.certificate_type,
        proposer,
    })
}

/// Read a certificate range and attach each certificate's proposer.
///
/// Resolving proposers can turn a complete page into an unavailable one, so
/// the page is rebuilt from the resolved result rather than carried through.
async fn certificate_range_page(
    range: HeightRangeRequest,
    ctx: &AppRequestContext,
    is_reply_closed: impl Fn() -> bool,
) -> eyre::Result<Option<HeightRangePage<CommitCertificateInfo>>> {
    let Some(page) = ctx
        .store
        .get_certificate_range(range.from, range.count, MAX_RANGE_QUERY_BYTES)
        .await
        .wrap_err_with(|| {
            format!("GetCertificateRange: Failed to get certificates for {range:?}")
        })?
    else {
        return Ok(None);
    };

    let (result, next) = page.into_parts();
    let resolved = match result {
        RangeQueryResult::Unavailable {
            reason,
            failed_heights,
        } => RangeQueryResult::Unavailable {
            reason,
            failed_heights,
        },
        RangeQueryResult::Complete(certs) => {
            let Some(resolved) = get_certificates_info(
                ctx.proposer_selector,
                &ctx.engine,
                &ctx.metrics,
                certs,
                &is_reply_closed,
            )
            .await
            else {
                return Ok(None);
            };
            resolved
        }
    };

    Ok(Some(HeightRangePage::new(resolved, next)))
}

/// Resolve proposers for a complete range of stored certificates.
///
/// Reuses [`get_certificate_info`], so the validator-set lookup only runs
/// for legacy rows without a stored proposer. Heights whose lookup fails
/// become an `Internal` range failure.
///
/// Returns `None` when `is_reply_closed` reports that the caller has gone
/// away, before the next execution-layer lookup. The caller discards that
/// outcome instead of replying with a partial page.
async fn get_certificates_info(
    proposer_selector: impl ProposerSelector + Copy,
    engine: &Engine,
    metrics: &AppMetrics,
    certs: Vec<StoredCommitCertificate>,
    is_reply_closed: impl Fn() -> bool,
) -> Option<RangeQueryResult<CommitCertificateInfo>> {
    let mut infos = Vec::with_capacity(certs.len());
    let mut failed_heights = Vec::new();

    // TODO(perf): if ranging over legacy (proposer == None) rows becomes a
    // bottleneck, batch these eth_call valset lookups like get_execution_payloads.
    // Only reachable for pre-migration rows on an archive EL; bounded by the
    // MAX_RANGE_COUNT constant.
    for stored in certs {
        if is_reply_closed() {
            return None;
        }

        let height = stored.certificate.height;
        match get_certificate_info(proposer_selector, engine, metrics, stored).await {
            Some(info) => infos.push(info),
            None => failed_heights.push(height),
        }
    }

    Some(if failed_heights.is_empty() {
        RangeQueryResult::Complete(infos)
    } else {
        RangeQueryResult::Unavailable {
            reason: RangeFailureReason::Internal,
            failed_heights,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, SystemTime};

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3};
    use arc_consensus_db::{DbUpgrade, Store};
    use arc_consensus_types::signing::PrivateKey;
    use arc_consensus_types::{
        Address, BlockHash, CommitCertificate, CommitCertificateType, Height, Round, Validator,
        ValidatorSet, ValueId,
    };
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};
    use arc_eth_engine::json_structures::ExecutionBlock;
    use bytesize::ByteSize;
    use mockall::predicate::eq;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    use crate::metrics::DbMetrics;
    use crate::request::CONSENSUS_LOOP_LIVENESS;
    use crate::state::StatusSnapshot;
    use crate::utils::sync_state::SyncState;

    fn stored_cert(height: u64, proposer: Option<Address>) -> StoredCommitCertificate {
        StoredCommitCertificate {
            certificate: CommitCertificate::new(
                Height::new(height),
                Round::new(0),
                ValueId::new(BlockHash::new([0xAA; 32])),
                vec![],
            ),
            certificate_type: CommitCertificateType::Minimal,
            proposer,
        }
    }

    fn validator_set() -> ValidatorSet {
        let mut rng = StdRng::seed_from_u64(0x42);
        let signing_key = PrivateKey::generate(&mut rng);
        ValidatorSet::new(vec![Validator::new(signing_key.public_key(), 1)])
    }

    #[tokio::test]
    async fn get_certificate_info_uses_stored_proposer_without_validator_set_lookup() {
        let stored_proposer = Address::new([0x42; 20]);
        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let info = get_certificate_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            stored_cert(42, Some(stored_proposer)),
        )
        .await
        .expect("should return Some");

        assert_eq!(info.proposer, stored_proposer);
        assert_eq!(info.certificate.height, Height::new(42));
    }

    /// get_certificate_info passes the certificate's consensus height directly;
    /// the EthereumAPI impl is responsible for querying at `consensus_height - 1`.
    #[tokio::test]
    async fn get_certificate_info_queries_signing_validator_set_when_proposer_is_missing() {
        let cert_height = 42u64;
        let fallback_validator_set = validator_set();
        let expected_proposer = fallback_validator_set
            .get_by_index(0)
            .expect("test validator set is non-empty")
            .address;

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_signing_validator_set()
            .with(eq(cert_height))
            .once()
            .returning(move |_| Ok(fallback_validator_set.clone()));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let info = get_certificate_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            stored_cert(cert_height, None),
        )
        .await
        .expect("should return Some");

        assert_eq!(info.certificate.height, Height::new(cert_height));
        assert_eq!(info.proposer, expected_proposer);
    }

    #[tokio::test]
    async fn get_certificates_info_skips_engine_when_proposers_are_stored() {
        let proposer = Address::new([0x42; 20]);
        // No mock expectations: any engine call would panic.
        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let result = get_certificates_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            vec![
                stored_cert(1, Some(proposer)),
                stored_cert(2, Some(proposer)),
                stored_cert(3, Some(proposer)),
            ],
            || false,
        )
        .await
        .expect("reply stayed open");

        let RangeQueryResult::Complete(infos) = result else {
            panic!("expected complete range");
        };
        let heights: Vec<u64> = infos
            .iter()
            .map(|i| i.certificate.height.as_u64())
            .collect();
        assert_eq!(heights, vec![1, 2, 3]);
        assert!(infos.iter().all(|i| i.proposer == proposer));
    }

    #[tokio::test]
    async fn get_certificates_info_resolves_legacy_rows_via_validator_set() {
        let stored_proposer = Address::new([0x42; 20]);
        let fallback_validator_set = validator_set();
        let expected_fallback = fallback_validator_set
            .get_by_index(0)
            .expect("test validator set is non-empty")
            .address;

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_signing_validator_set()
            .with(eq(2u64))
            .once()
            .returning(move |_| Ok(fallback_validator_set.clone()));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let result = get_certificates_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            vec![stored_cert(1, Some(stored_proposer)), stored_cert(2, None)],
            || false,
        )
        .await
        .expect("reply stayed open");

        let RangeQueryResult::Complete(infos) = result else {
            panic!("expected complete range");
        };
        assert_eq!(infos[0].proposer, stored_proposer);
        assert_eq!(infos[1].proposer, expected_fallback);
    }

    #[tokio::test]
    async fn get_certificates_info_reports_failed_lookups_as_internal() {
        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_signing_validator_set()
            .with(eq(2u64))
            .once()
            .returning(|_| Err(eyre!("validator set unavailable")));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let result = get_certificates_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            vec![
                stored_cert(1, Some(Address::new([0x42; 20]))),
                stored_cert(2, None),
            ],
            || false,
        )
        .await
        .expect("reply stayed open");

        let RangeQueryResult::Unavailable {
            reason,
            failed_heights,
        } = result
        else {
            panic!("expected unavailable range");
        };
        assert_eq!(reason, RangeFailureReason::Internal);
        assert_eq!(failed_heights, vec![Height::new(2)]);
    }

    fn test_snapshot() -> StatusSnapshot {
        let key = PrivateKey::from([7u8; 32]);
        let public_key = key.public_key();
        let address = Address::from_public_key(&public_key);
        StatusSnapshot {
            height: Height::new(1),
            round: Round::new(0),
            proposer: None,
            address,
            public_key,
            previous_block: None,
            validator_set: validator_set(),
            sync_state: crate::utils::sync_state::SyncState::InSync,
        }
    }

    async fn test_ctx() -> (AppRequestContext, tempfile::TempDir) {
        use arc_consensus_db::{DbMetrics, DbUpgrade};
        use bytesize::ByteSize;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .unwrap();
        let (_status_tx, status_rx) = watch::channel(test_snapshot());
        let ctx = AppRequestContext {
            store,
            engine: Engine::new(
                Box::new(MockEngineAPI::new()),
                Box::new(MockEthereumAPI::new()),
            ),
            stats: Stats::default(),
            metrics: AppMetrics::default(),
            proposer_selector: RoundRobin,
            status_rx,
            heartbeat_rx: watch::channel(Some(Instant::now())).1,
        };
        (ctx, dir)
    }

    #[test]
    fn reply_or_log_swallows_a_dropped_receiver() {
        let (tx, rx) = oneshot::channel();
        drop(rx);
        reply_or_log(tx, (), "GetHealth");
    }

    #[tokio::test]
    async fn handle_app_request_times_every_variant() {
        let (ctx, _dir) = test_ctx().await;
        let enqueued_at = std::time::Instant::now();

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetCertificate {
                height: Some(Height::new(1)),
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetMisbehaviorEvidence {
                height: Some(Height::new(1)),
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetProposalMonitorData {
                height: Some(Height::new(1)),
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetInvalidPayloads {
                height: Some(Height::new(1)),
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetCertificateRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetMisbehaviorEvidenceRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetProposalMonitorDataRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetInvalidPayloadsRange {
                request: InvalidPayloadsRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_none());

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetStatus {
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        let status = rx.await.unwrap();
        assert_eq!(status.height, Height::new(1));
        assert_eq!(
            status.sync_state,
            crate::utils::sync_state::SyncState::InSync
        );

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetHealth {
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        rx.await.unwrap();

        let (tx, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetSyncState {
                enqueued_at,
                reply: tx,
            },
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            rx.await.unwrap(),
            crate::utils::sync_state::SyncState::InSync
        );

        let encoded = ctx.metrics.encode_app_request_times();
        for label in [
            AppRequest::LABEL_GET_CERTIFICATE,
            AppRequest::LABEL_GET_MISBEHAVIOR_EVIDENCE,
            AppRequest::LABEL_GET_PROPOSAL_MONITOR_DATA,
            AppRequest::LABEL_GET_INVALID_PAYLOADS,
            AppRequest::LABEL_GET_CERTIFICATE_RANGE,
            AppRequest::LABEL_GET_MISBEHAVIOR_EVIDENCE_RANGE,
            AppRequest::LABEL_GET_PROPOSAL_MONITOR_DATA_RANGE,
            AppRequest::LABEL_GET_INVALID_PAYLOADS_RANGE,
            AppRequest::LABEL_GET_STATUS,
            AppRequest::LABEL_GET_HEALTH,
            AppRequest::LABEL_GET_SYNC_STATE,
        ] {
            assert!(
                encoded.contains(&format!("request=\"{label}\"")),
                "queue/process times must record {label}, got:\n{encoded}"
            );
        }
    }

    #[tokio::test]
    async fn get_certificates_info_stops_once_reply_closes_mid_loop() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let closed = Arc::new(AtomicBool::new(false));
        let closed_in_lookup = closed.clone();

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_signing_validator_set()
            .times(1)
            .returning(move |_| {
                closed_in_lookup.store(true, Ordering::SeqCst);
                Ok(validator_set())
            });

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let result = get_certificates_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            vec![
                stored_cert(1, None),
                stored_cert(2, None),
                stored_cert(3, None),
            ],
            || closed.load(Ordering::SeqCst),
        )
        .await;

        assert!(
            result.is_none(),
            "heights after the closed reply must not be resolved"
        );
    }

    #[tokio::test]
    async fn liveness_probe_completes_while_store_request_is_blocked() {
        use std::time::{Duration, Instant};
        use tokio::sync::{mpsc, oneshot, Mutex};

        let (tx, rx) = mpsc::channel(8);
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));

        let handle = {
            let release_rx = release_rx.clone();
            move |req: AppRequest| {
                let release_rx = release_rx.clone();
                async move {
                    match req {
                        AppRequest::GetCertificate { reply, .. } => {
                            if let Some(gate) = release_rx.lock().await.take() {
                                let _ = gate.await;
                            }
                            let _ = reply.send(None);
                        }
                        AppRequest::GetHealth { reply, .. } => {
                            let _ = reply.send(Health::Ok);
                        }
                        _ => {}
                    }
                    Ok(())
                }
            }
        };

        let dispatcher = tokio::spawn(dispatch_app_requests(rx, handle, AppMetrics::default()));

        let (slow_tx, mut slow_rx) = oneshot::channel();
        tx.send(AppRequest::GetCertificate {
            height: None,
            enqueued_at: Instant::now(),
            reply: slow_tx,
        })
        .await
        .expect("enqueue store request");
        tokio::task::yield_now().await;

        let (health_tx, health_rx) = oneshot::channel();
        tx.send(AppRequest::GetHealth {
            enqueued_at: std::time::Instant::now(),
            reply: health_tx,
        })
        .await
        .expect("enqueue probe");

        tokio::time::timeout(Duration::from_millis(200), health_rx)
            .await
            .expect("GetHealth must finish while the store request is still blocked")
            .expect("health reply");

        assert!(
            slow_rx.try_recv().is_err(),
            "store request must still be waiting on the release gate"
        );

        release_tx.send(()).expect("release store request");
        drop(tx);
        let _ = dispatcher.await;
    }

    #[tokio::test]
    async fn spawned_handler_panic_fails_the_dispatcher() {
        use std::time::{Duration, Instant};
        use tokio::sync::{mpsc, oneshot};

        let (tx, rx) = mpsc::channel(8);
        let handle = |req: AppRequest| async move {
            match req {
                AppRequest::GetCertificate { .. } => panic!("store handler boom"),
                _ => Ok(()),
            }
        };

        let dispatcher = tokio::spawn(dispatch_app_requests(rx, handle, AppMetrics::default()));

        let (slow_tx, _slow_rx) = oneshot::channel();
        tx.send(AppRequest::GetCertificate {
            height: None,
            enqueued_at: Instant::now(),
            reply: slow_tx,
        })
        .await
        .expect("enqueue store request");

        let join = tokio::time::timeout(Duration::from_secs(1), dispatcher)
            .await
            .expect("dispatcher should exit after a spawned panic")
            .expect("dispatcher task");
        let err = join.err().expect("panic must fail dispatch_app_requests");
        assert!(
            format!("{err:#}").contains("panicked"),
            "error should mention the panic: {err:#}"
        );
    }

    #[tokio::test]
    async fn closed_reply_waiting_for_a_slot_does_not_run() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};
        use tokio::sync::{mpsc, oneshot, watch};

        let (tx, rx) = mpsc::channel(8);
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = watch::channel(false);

        let handle = {
            let started = started.clone();
            let finished = finished.clone();
            move |req: AppRequest| {
                let started = started.clone();
                let finished = finished.clone();
                let mut release_rx = release_rx.clone();
                async move {
                    if let AppRequest::GetCertificate { reply, .. } = req {
                        started.fetch_add(1, Ordering::SeqCst);
                        release_rx
                            .wait_for(|released| *released)
                            .await
                            .expect("release sender stays open");
                        finished.fetch_add(1, Ordering::SeqCst);
                        let _ = reply.send(None);
                    }
                    Ok(())
                }
            }
        };

        let metrics = AppMetrics::default();
        let dispatcher = tokio::spawn(dispatch_app_requests(rx, handle, metrics.clone()));

        let mut holders = Vec::new();
        for _ in 0..MAX_CONCURRENT_STORE_REQUESTS {
            let (reply, rx_reply) = oneshot::channel();
            holders.push(rx_reply);
            tx.send(AppRequest::GetCertificate {
                height: None,
                enqueued_at: Instant::now(),
                reply,
            })
            .await
            .expect("enqueue store request");
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while started.load(Ordering::SeqCst) < MAX_CONCURRENT_STORE_REQUESTS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("every store slot should be taken");

        let (abandoned_tx, abandoned_rx) = oneshot::channel();
        tx.send(AppRequest::GetCertificate {
            height: None,
            enqueued_at: Instant::now(),
            reply: abandoned_tx,
        })
        .await
        .expect("enqueue abandoned store request");
        tokio::task::yield_now().await;
        drop(abandoned_rx);
        tokio::time::sleep(Duration::from_millis(50)).await;

        release_tx.send(true).expect("release store slots");
        tokio::time::timeout(Duration::from_secs(1), async {
            while finished.load(Ordering::SeqCst) < MAX_CONCURRENT_STORE_REQUESTS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("holders should finish once released");
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            started.load(Ordering::SeqCst),
            MAX_CONCURRENT_STORE_REQUESTS,
            "a closed reply waiting on a slot must not run the handler"
        );
        assert_eq!(
            metrics.app_request_skipped_count(AppRequest::LABEL_GET_CERTIFICATE),
            1,
            "a caller that gives up while waiting for a slot must be counted"
        );

        let (live_tx, live_rx) = oneshot::channel();
        tx.send(AppRequest::GetCertificate {
            height: None,
            enqueued_at: Instant::now(),
            reply: live_tx,
        })
        .await
        .expect("enqueue live store request");
        tokio::time::timeout(Duration::from_secs(1), live_rx)
            .await
            .expect("a live request still gets a slot after an abandoned one drops out")
            .expect("live reply");
        assert_eq!(
            started.load(Ordering::SeqCst),
            MAX_CONCURRENT_STORE_REQUESTS + 1
        );
        assert_eq!(
            holders.len(),
            MAX_CONCURRENT_STORE_REQUESTS,
            "slot holders must stay open until the live request has a slot"
        );

        drop(tx);
        let _ = dispatcher.await;
    }

    async fn test_context(
        engine: Engine,
        metrics: AppMetrics,
    ) -> (AppRequestContext, tempfile::TempDir) {
        use arc_consensus_db::{DbMetrics, DbUpgrade};
        use bytesize::ByteSize;

        let dir = tempfile::tempdir().expect("create temp dir for test store");
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            ByteSize::mib(64),
        )
        .await
        .expect("open test store");

        let key = PrivateKey::from([7u8; 32]);
        let public_key = key.public_key();
        let address = Address::from_public_key(&public_key);
        let (_status_tx, status_rx) = watch::channel(StatusSnapshot {
            height: Height::new(1),
            round: Round::new(0),
            proposer: None,
            address,
            public_key,
            previous_block: None,
            validator_set: ValidatorSet::new(vec![Validator::new(public_key, 1)]),
            sync_state: crate::utils::sync_state::SyncState::InSync,
        });

        let ctx = AppRequestContext {
            store,
            engine,
            stats: Stats::default(),
            metrics,
            proposer_selector: ArcContext::default().proposer_selector,
            status_rx,
            heartbeat_rx: watch::channel(Some(Instant::now())).1,
        };

        (ctx, dir)
    }

    #[tokio::test]
    async fn handle_app_request_skips_get_certificate_once_reply_is_closed() {
        use std::time::Instant;
        use tokio::sync::oneshot;

        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );
        let metrics = AppMetrics::default();
        let (ctx, _dir) = test_context(engine, metrics.clone()).await;

        let (reply, rx) = oneshot::channel();
        drop(rx);

        handle_app_request(
            AppRequest::GetCertificate {
                height: None,
                enqueued_at: Instant::now(),
                reply,
            },
            &ctx,
        )
        .await
        .expect("a closed reply must not surface as an error");

        assert!(
            !metrics.has_app_request_queue_time_observation(AppRequest::LABEL_GET_CERTIFICATE),
            "no work should have been recorded once the caller already gave up"
        );
        assert_eq!(
            metrics.app_request_skipped_count(AppRequest::LABEL_GET_CERTIFICATE),
            1
        );
    }

    #[tokio::test]
    async fn handle_app_request_records_certificate_range_queue_time() {
        use std::time::Instant;
        use tokio::sync::oneshot;

        let engine = Engine::new(
            Box::new(MockEngineAPI::new()),
            Box::new(MockEthereumAPI::new()),
        );
        let metrics = AppMetrics::default();
        let (ctx, _dir) = test_context(engine, metrics.clone()).await;

        let (reply, rx) = oneshot::channel();
        handle_app_request(
            AppRequest::GetCertificateRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 1,
                },
                enqueued_at: Instant::now(),
                reply,
            },
            &ctx,
        )
        .await
        .expect("an empty store still serves the range request");

        assert!(
            rx.await.expect("range reply").is_none(),
            "an empty store has no certificate page"
        );
        assert!(
            metrics.has_app_request_queue_time_observation(AppRequest::LABEL_GET_CERTIFICATE_RANGE),
            "range requests must record queue time"
        );
        assert_eq!(
            metrics.app_request_skipped_count(AppRequest::LABEL_GET_CERTIFICATE_RANGE),
            0
        );
    }

    struct TestApp {
        ctx: AppRequestContext,
        heartbeat_tx: watch::Sender<Option<Instant>>,
        _dir: tempfile::TempDir,
    }

    impl TestApp {
        async fn open() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let store = Store::open(
                dir.path().join("db"),
                DbMetrics::default(),
                DbUpgrade::Skip,
                ByteSize::mib(64),
            )
            .await
            .expect("store opens");

            let key = PrivateKey::from([7u8; 32]);
            let public_key = key.public_key();
            let now = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after unix epoch")
                .as_secs();
            let (_status_tx, status_rx) = watch::channel(StatusSnapshot {
                height: Height::new(1),
                round: Round::new(0),
                proposer: None,
                address: Address::from_public_key(&public_key),
                public_key,
                previous_block: Some(ExecutionBlock {
                    block_hash: BlockHash::repeat_byte(1),
                    block_number: 1,
                    parent_hash: BlockHash::repeat_byte(0),
                    timestamp: now,
                }),
                validator_set: ValidatorSet::new(vec![Validator::new(public_key, 1)]),
                sync_state: SyncState::InSync,
            });
            let (heartbeat_tx, heartbeat_rx) = watch::channel(Some(Instant::now()));

            Self {
                ctx: AppRequestContext {
                    store,
                    engine: Engine::new(
                        Box::new(MockEngineAPI::new()),
                        Box::new(MockEthereumAPI::new()),
                    ),
                    stats: Stats::default(),
                    metrics: AppMetrics::default(),
                    proposer_selector: ArcContext::default().proposer_selector,
                    status_rx,
                    heartbeat_rx,
                },
                heartbeat_tx,
                _dir: dir,
            }
        }
    }

    fn execution_payload(height: u64, block_hash: BlockHash) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    parent_hash: BlockHash::repeat_byte(0),
                    fee_recipient: AlloyAddress::ZERO,
                    state_root: BlockHash::repeat_byte(1),
                    receipts_root: BlockHash::repeat_byte(2),
                    logs_bloom: Bloom::default(),
                    prev_randao: BlockHash::repeat_byte(3),
                    block_number: height,
                    gas_limit: 30_000_000,
                    gas_used: 0,
                    timestamp: 1,
                    extra_data: AlloyBytes::default(),
                    base_fee_per_gas: U256::from(1u64),
                    block_hash,
                    transactions: vec![],
                },
                withdrawals: vec![],
            },
            blob_gas_used: 0,
            excess_blob_gas: 0,
        }
    }

    async fn reply<T>(ctx: &AppRequestContext, request: AppRequest, rx: oneshot::Receiver<T>) -> T {
        handle_app_request(request, ctx)
            .await
            .expect("app request succeeds");
        rx.await.expect("handler replied")
    }

    #[tokio::test]
    async fn app_requests_on_an_empty_store_report_health_and_sync() {
        let app = TestApp::open().await;
        let ctx = &app.ctx;
        let height = Some(Height::new(1));

        let (tx, rx) = oneshot::channel();
        let certificate = reply(
            ctx,
            AppRequest::GetCertificate {
                height,
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await;
        assert!(certificate.is_none());

        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetMisbehaviorEvidence {
                height,
                enqueued_at: std::time::Instant::now(),
                reply: tx
            },
            rx
        )
        .await
        .is_none());

        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetProposalMonitorData {
                height,
                enqueued_at: std::time::Instant::now(),
                reply: tx
            },
            rx
        )
        .await
        .is_none());

        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetInvalidPayloads {
                height,
                enqueued_at: std::time::Instant::now(),
                reply: tx
            },
            rx
        )
        .await
        .is_none());

        let range = || HeightRangeRequest {
            from: Height::new(1),
            count: 1,
        };
        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetCertificateRange {
                range: range(),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx
        )
        .await
        .is_none());
        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetMisbehaviorEvidenceRange {
                range: range(),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx
        )
        .await
        .is_none());
        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetProposalMonitorDataRange {
                range: range(),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx
        )
        .await
        .is_none());
        let (tx, rx) = oneshot::channel();
        assert!(reply(
            ctx,
            AppRequest::GetInvalidPayloadsRange {
                request: InvalidPayloadsRequest {
                    from: Height::new(1),
                    count: 1,
                },
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await
        .is_none());

        let (tx, rx) = oneshot::channel();
        let status = reply(
            ctx,
            AppRequest::GetStatus {
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await;
        assert_eq!(status.height, Height::new(1));
        assert_eq!(status.sync_state, SyncState::InSync);

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            reply(
                ctx,
                AppRequest::GetHealth {
                    enqueued_at: std::time::Instant::now(),
                    reply: tx
                },
                rx
            )
            .await,
            Health::Ok
        );

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            reply(
                ctx,
                AppRequest::GetSyncState {
                    enqueued_at: std::time::Instant::now(),
                    reply: tx
                },
                rx
            )
            .await,
            SyncState::InSync
        );
    }

    #[tokio::test]
    async fn app_requests_return_a_stored_certificate_and_empty_sidecar_rows() {
        let app = TestApp::open().await;
        let proposer = Address::new([0x42; 20]);
        let height = Height::new(1);
        let block_hash = BlockHash::repeat_byte(0xAA);
        app.ctx
            .store
            .store_decided_block(
                CommitCertificate::new(height, Round::new(0), ValueId::new(block_hash), vec![]),
                execution_payload(1, block_hash),
                proposer,
            )
            .await
            .expect("decided block stored");

        let (tx, rx) = oneshot::channel();
        let info = reply(
            &app.ctx,
            AppRequest::GetCertificate {
                height: Some(height),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await
        .expect("stored certificate");
        assert_eq!(info.proposer, proposer);
        assert_eq!(info.certificate.height, height);

        let (tx, rx) = oneshot::channel();
        let evidence = reply(
            &app.ctx,
            AppRequest::GetMisbehaviorEvidence {
                height: Some(height),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await
        .expect("finalized height has an evidence row");
        assert_eq!(evidence.height, height);
        assert!(evidence.validators.is_empty());

        let (tx, rx) = oneshot::channel();
        let payloads = reply(
            &app.ctx,
            AppRequest::GetInvalidPayloads {
                height: Some(height),
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await
        .expect("finalized height has an invalid-payload row");
        assert_eq!(payloads.height, height);
        assert!(payloads.payloads.is_empty());

        let (tx, rx) = oneshot::channel();
        let page = reply(
            &app.ctx,
            AppRequest::GetCertificateRange {
                range: HeightRangeRequest {
                    from: height,
                    count: 1,
                },
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            rx,
        )
        .await
        .expect("certificate page");
        let (result, next) = page.into_parts();
        let RangeQueryResult::Complete(infos) = result else {
            panic!("expected a complete certificate page");
        };
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].proposer, proposer);
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn health_request_reports_a_stale_heartbeat() {
        let app = TestApp::open().await;
        let stale = Instant::now()
            .checked_sub(CONSENSUS_LOOP_LIVENESS + Duration::from_millis(1))
            .expect("monotonic clock has more than 60s of history");
        app.heartbeat_tx
            .send(Some(stale))
            .expect("heartbeat updates");

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            reply(
                &app.ctx,
                AppRequest::GetHealth {
                    enqueued_at: std::time::Instant::now(),
                    reply: tx
                },
                rx
            )
            .await,
            Health::EventLoopStalled
        );
    }

    #[tokio::test]
    async fn health_request_is_ok_before_consensus_ready_completes() {
        let app = TestApp::open().await;
        app.heartbeat_tx.send(None).expect("heartbeat updates");

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            reply(
                &app.ctx,
                AppRequest::GetHealth {
                    enqueued_at: std::time::Instant::now(),
                    reply: tx,
                },
                rx
            )
            .await,
            Health::Ok
        );
    }

    #[tokio::test]
    async fn health_request_ignores_a_dropped_reply() {
        let app = TestApp::open().await;
        let (tx, rx) = oneshot::channel();
        drop(rx);
        handle_app_request(
            AppRequest::GetHealth {
                enqueued_at: std::time::Instant::now(),
                reply: tx,
            },
            &app.ctx,
        )
        .await
        .expect("a dropped reply is not a handler failure");
    }
}
