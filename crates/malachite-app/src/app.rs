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
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use malachitebft_app_channel::{AppMsg, Channels};
use malachitebft_core_types::utils::height::DisplayRange;
use malachitebft_core_types::Context;

use arc_consensus_types::{ArcContext, StoredCommitCertificate};
use arc_eth_engine::engine::Engine;

use crate::handlers::*;
use crate::request::{AppRequest, CommitCertificateInfo};
use crate::state::State;

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

    let result = cancel_token
        .run_until_cancelled_owned(go(&mut state, channels, &engine, rx_app_req))
        .await;

    let result = match result {
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

/// The main event loop of the application.
///
/// It listens for messages from consensus and application requests,
/// and dispatches them to the appropriate handlers.
///
/// # Errors
/// Returns an error if handling a message fails or one of the channels is closed unexpectedly.
/// This will cause the application to shut down.
/// Otherwise, it runs indefinitely until cancelled.
async fn go(
    state: &mut State,
    mut channels: Channels<ArcContext>,
    engine: &Engine,
    mut rx_app_req: Receiver<AppRequest>,
) -> eyre::Result<Never> {
    loop {
        tokio::select! {
            biased;

            msg = channels.consensus.recv() => match msg {
                Some(msg) => {
                    // Abort on error to shut down the application.
                    handle_consensus(msg, state, &mut channels, engine).await
                        .wrap_err("Error handling consensus message")?;
                },
                None => {
                    return Err(eyre!("Consensus channel closed unexpectedly"));
                }
            },

            req = rx_app_req.recv() => match req {
                Some(req) => {
                    if let Err(e) = handle_app_request(req, state, engine).await {
                        error!("🔴 Error handling application request: {e:#}");

                        // We continue processing other requests even if one fails.
                        continue;
                    }
                },
                None => {
                    return Err(eyre!("Application request channel closed unexpectedly"));
                }
            }
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
        AppMsg::Decided { certificate, .. } => {
            let _guard = state.metrics.start_msg_process_timer("Decided");

            let height = certificate.height;
            let round = certificate.round;
            let value_id = certificate.value_id;
            let signatures = certificate.commit_signatures.len();

            info!(%height, %round, %value_id, %signatures, "🎉 Consensus has decided on value");

            decided::handle(state, engine, certificate).await?;
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

#[allow(clippy::unit_arg)]
async fn handle_app_request(req: AppRequest, state: &State, engine: &Engine) -> eyre::Result<()> {
    match req {
        AppRequest::GetCertificate(height, reply) => {
            let result = state
                .store()
                .get_certificate(height)
                .await
                .wrap_err_with(|| {
                    format!("GetCertificate: Failed to get certificate for height {height:?}")
                })?;

            let info = match result {
                Some(certificate) => get_certificate_info(&state.ctx, engine, certificate).await,
                None => None,
            };

            if let Err(e) = reply.send(info) {
                error!("GetCertificate: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetMisbehaviorEvidence(height, reply) => {
            let evidence = state.store().get_misbehavior_evidence(height).await.wrap_err_with(|| {
                format!(
                    "GetMisbehaviorEvidence: Failed to get misbehavior evidence for height {height:?}",
                )
            })?;
            if let Err(e) = reply.send(evidence) {
                error!("GetMisbehaviorEvidence: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetProposalMonitorData(height, reply) => {
            let data = state
                .store()
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
            let payloads = state.store().get_invalid_payloads(height).await.wrap_err_with(|| {
                format!(
                    "Failed to get invalid payloads for height {:?} in response to a GetInvalidPayloads request", height,
                )
            })?;
            if let Err(e) = reply.send(payloads) {
                error!("Failed to reply to GetInvalidPayloads: {e:?}");
            }
        }

        AppRequest::GetStatus(reply) => {
            let status = state
                .get_status()
                .await
                .wrap_err("GetStatus: Failed to get the current status")?;

            if let Err(e) = reply.send(status) {
                error!("GetStatus: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetHealth(reply) => {
            if let Err(e) = reply.send(state.get_health()) {
                error!("GetHealth: Failed to reply: {e:?}");
            }
        }

        AppRequest::GetSyncState(reply) => {
            if let Err(e) = reply.send(state.sync_state) {
                error!("GetSyncState: Failed to reply: {e:?}");
            }
        }
    }

    Ok(())
}

async fn get_certificate_info(
    ctx: &ArcContext,
    engine: &Engine,
    stored: StoredCommitCertificate,
) -> Option<CommitCertificateInfo> {
    // The validator set that signed the certificate is the one *before* executing that block,
    // since the block itself could contain validator set changes.
    let prev_height = stored.certificate.height.as_u64().saturating_sub(1);
    let validator_set = engine
        .eth
        .get_active_validator_set(prev_height)
        .await
        .ok()?;

    let proposer = stored.proposer.unwrap_or_else(|| {
        ctx.select_proposer(
            &validator_set,
            stored.certificate.height,
            stored.certificate.round,
        )
        .address
    });

    Some(CommitCertificateInfo {
        certificate: stored.certificate,
        certificate_type: stored.certificate_type,
        proposer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use arc_consensus_types::{
        Address, BlockHash, CommitCertificate, CommitCertificateType, Height, Round, ValueId,
    };
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};
    use mockall::predicate::eq;

    fn stored_cert(height: u64) -> StoredCommitCertificate {
        StoredCommitCertificate {
            certificate: CommitCertificate::new(
                Height::new(height),
                Round::new(0),
                ValueId::new(BlockHash::new([0xAA; 32])),
                vec![],
            ),
            certificate_type: CommitCertificateType::Minimal,
            // Set so get_certificate_info skips select_proposer (keeps the test focused
            // on the validator set lookup).
            proposer: Some(Address::new([0x42; 20])),
        }
    }

    /// get_certificate_info must fetch the validator set at `certificate.height - 1` — the
    /// set that signed the certificate, i.e. the state *before* executing the certified block.
    #[tokio::test]
    async fn get_certificate_info_queries_validator_set_at_prev_height() {
        let cert_height = 42u64;

        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_active_validator_set()
            .with(eq(cert_height - 1))
            .once()
            .returning(|_| Ok(Default::default()));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let ctx = ArcContext::default();

        let info = get_certificate_info(&ctx, &engine, stored_cert(cert_height))
            .await
            .expect("should return Some");

        assert_eq!(info.certificate.height, Height::new(cert_height));
    }

    /// At genesis (height 0), the saturating subtraction must keep the query at 0 rather
    /// than underflowing.
    #[tokio::test]
    async fn get_certificate_info_handles_genesis_height() {
        let mut mock_eth = MockEthereumAPI::new();
        mock_eth
            .expect_get_active_validator_set()
            .with(eq(0u64))
            .once()
            .returning(|_| Ok(Default::default()));

        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(mock_eth));
        let ctx = ArcContext::default();

        let info = get_certificate_info(&ctx, &engine, stored_cert(0))
            .await
            .expect("should return Some");

        assert_eq!(info.certificate.height, Height::new(0));
    }
}
