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

use eyre::{eyre, Context as _};
use tokio::sync::mpsc::Receiver;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use malachitebft_app_channel::{AppMsg, Channels};
use malachitebft_core_types::utils::height::DisplayRange;

use arc_consensus_types::proposer::{ProposerSelector, RoundRobin};
use arc_consensus_types::{ArcContext, StoredCommitCertificate};
use arc_eth_engine::engine::Engine;

use crate::handlers::*;
use crate::metrics::AppMetrics;
use crate::request::{AppRequest, CommitCertificateInfo};
use crate::state::{State, StatusSnapshot};
use crate::stats::Stats;
use crate::store::Store;

pub async fn run(
    mut state: State,
    channels: Channels<ArcContext>,
    engine: Engine,
    rx_app_req: Receiver<AppRequest>,
    cancel_token: CancellationToken,
) -> eyre::Result<()> {
    if let Some(halt_height) = state.env_config().halt_height {
        warn!("Consensus configured to halt at block height: {halt_height}");
    }

    let (status_tx, status_rx) = watch::channel(state.status_snapshot());

    let ctx = AppRequestContext {
        store: state.store().clone(),
        engine: engine.clone(),
        stats: state.stats().clone(),
        metrics: state.metrics().clone(),
        proposer_selector: state.ctx.proposer_selector,
        status_rx,
    };

    let mut app_req_task: JoinHandle<eyre::Result<Never>> =
        tokio::spawn(process_app_requests(rx_app_req, ctx));

    let result = cancel_token
        .run_until_cancelled_owned(go(
            &mut state,
            channels,
            &engine,
            status_tx,
            &mut app_req_task,
        ))
        .await;

    // Always abort the app-request task, including on graceful cancellation
    // where `go` is dropped at an await point and would otherwise detach it.
    app_req_task.abort();

    let result = match result {
        Some(Ok(never)) => match never {},
        Some(Err(e)) => {
            error!("🔴 Error in application: {e:#}");
            error!("🔴 Shutting down");
            Err(e)
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
struct AppRequestContext {
    store: Store,
    engine: Engine,
    stats: Stats,
    metrics: AppMetrics,
    proposer_selector: RoundRobin,
    status_rx: watch::Receiver<StatusSnapshot>,
}

/// Runs in a dedicated tokio task, processing app requests concurrently
/// with the consensus event loop.
async fn process_app_requests(
    mut rx: Receiver<AppRequest>,
    ctx: AppRequestContext,
) -> eyre::Result<Never> {
    loop {
        match rx.recv().await {
            Some(req) => {
                if let Err(e) = handle_app_request(req, &ctx).await {
                    error!("🔴 Error handling application request: {e:#}");
                }
            }
            None => {
                return Err(eyre!("Application request channel closed unexpectedly"));
            }
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
    app_req_task: &mut JoinHandle<eyre::Result<Never>>,
) -> eyre::Result<Never> {
    loop {
        tokio::select! {
            biased;

            msg = channels.consensus.recv() => match msg {
                Some(msg) => {
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
                },
                None => {
                    return Err(eyre!("Consensus channel closed unexpectedly"));
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

            started_round::handle(state, engine, height, round, proposer, role, reply_value).await;
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
        AppMsg::ReceivedProposalPart { from, part, reply } => {
            let _guard = state
                .metrics
                .start_msg_process_timer("ReceivedProposalPart");

            received_proposal_part::handle(state, engine, from, part, reply).await;
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
        // Request to verify a vote extension
        AppMsg::VerifyVoteExtension { reply, .. } => {
            if let Err(e) = reply.send(Ok(())) {
                error!("🔴 Failed to send VerifyVoteExtension reply: {e:?}");
            }
        }
    }

    Ok(())
}

async fn handle_app_request(req: AppRequest, ctx: &AppRequestContext) -> eyre::Result<()> {
    match req {
        AppRequest::GetCertificate {
            height,
            enqueued_at,
            reply,
        } => {
            ctx.metrics.observe_app_request_queue_time(
                "GetCertificate",
                enqueued_at.elapsed().as_secs_f64(),
            );
            let _guard = ctx
                .metrics
                .start_app_request_process_timer("GetCertificate");

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

            if let Err(e) = reply.send(info) {
                error!("GetCertificate: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetMisbehaviorEvidence(height, reply) => {
            let evidence = ctx.store.get_misbehavior_evidence(height).await.wrap_err_with(|| {
                format!(
                    "GetMisbehaviorEvidence: Failed to get misbehavior evidence for height {height:?}",
                )
            })?;
            if let Err(e) = reply.send(evidence) {
                error!("GetMisbehaviorEvidence: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetProposalMonitorData(height, reply) => {
            let data = ctx.store
                .get_proposal_monitor_data(height)
                .await
                .wrap_err_with(|| {
                    format!(
                        "Failed to get proposal monitor data for height {:?} in response to a GetProposalMonitorData request",
                        height,
                    )
                })?;
            if let Err(e) = reply.send(data) {
                error!("Failed to reply to GetProposalMonitorData: {e:?}");
            }
        }

        AppRequest::GetInvalidPayloads(height, reply) => {
            let payloads = ctx.store.get_invalid_payloads(height).await.wrap_err_with(|| {
                format!(
                    "Failed to get invalid payloads for height {:?} in response to a GetInvalidPayloads request", height,
                )
            })?;
            if let Err(e) = reply.send(payloads) {
                error!("Failed to reply to GetInvalidPayloads: {e:?}");
            }
        }

        AppRequest::GetStatus(reply) => {
            let snapshot = ctx.status_rx.borrow().clone();
            let status = snapshot
                .get_status(&ctx.store, &ctx.stats)
                .await
                .wrap_err("GetStatus: Failed to get the current status")?;

            if let Err(e) = reply.send(status) {
                error!("GetStatus: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetHealth(reply) => {
            if let Err(e) = reply.send(()) {
                error!("GetHealth: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetSyncState(reply) => {
            let sync_state = ctx.status_rx.borrow().sync_state;
            if let Err(e) = reply.send(sync_state) {
                error!("GetSyncState: Failed to reply: {e:?}");
            }
        }
    }

    Ok(())
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

    // The validator set that signed the certificate is the one *before* executing that block,
    // since the block itself could contain validator set changes.
    let prev_height = stored.certificate.height.as_u64().saturating_sub(1);
    let validator_set = {
        let _guard =
            metrics.start_engine_api_timer("get_certificate_info.get_active_validator_set");
        engine
            .eth
            .get_active_validator_set(prev_height)
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

#[cfg(test)]
mod tests {
    use super::*;

    use arc_consensus_types::signing::PrivateKey;
    use arc_consensus_types::{
        Address, BlockHash, CommitCertificate, CommitCertificateType, Height, Round, Validator,
        ValidatorSet, ValueId,
    };
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};
    use mockall::predicate::eq;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

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

    /// get_certificate_info must fetch the validator set at `certificate.height - 1` — the
    /// set that signed the certificate, i.e. the state *before* executing the certified block.
    #[tokio::test]
    async fn get_certificate_info_queries_validator_set_at_prev_height_when_proposer_is_missing() {
        let cert_height = 42u64;
        let fallback_validator_set = validator_set();
        let expected_proposer = fallback_validator_set
            .get_by_index(0)
            .expect("test validator set is non-empty")
            .address;

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_active_validator_set()
            .with(eq(cert_height - 1))
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

    /// At the first certificate height, the previous-height validator-set lookup
    /// must query height 0.
    #[tokio::test]
    async fn get_certificate_info_queries_validator_set_at_zero_for_first_certificate_height() {
        let fallback_validator_set = validator_set();
        let expected_proposer = fallback_validator_set
            .get_by_index(0)
            .expect("test validator set is non-empty")
            .address;

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_active_validator_set()
            .with(eq(0u64))
            .once()
            .returning(move |_| Ok(fallback_validator_set.clone()));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let metrics = AppMetrics::default();
        let ctx = ArcContext::default();

        let info = get_certificate_info(
            ctx.proposer_selector,
            &engine,
            &metrics,
            stored_cert(1, None),
        )
        .await
        .expect("should return Some");

        assert_eq!(info.certificate.height, Height::new(1));
        assert_eq!(info.proposer, expected_proposer);
    }
}
