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

use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{mpsc, oneshot, watch};
use tracing::error;

use arc_consensus_db::invalid_payloads::StoredInvalidPayloads;
use arc_consensus_db::HeightRangePage;
use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::{
    signing::PublicKey, Address, ArcContext, BlockHash, CommitCertificateType, Height, Round,
    ValidatorSet,
};
use malachitebft_core_types::CommitCertificate;

use arc_consensus_types::proposal_monitor::ProposalMonitor;

use crate::utils::sync_state::SyncState;

/// Why an [`AppRequest`] round-trip to the consensus task failed.
///
/// The RPC layer maps these to HTTP statuses via `request_error_to_response`
/// (`Full` → 429, `Closed`/`Recv` → 500).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AppRequestError {
    /// The request channel is closed; the consensus task has stopped.
    Closed,
    /// The request channel is at capacity; the caller should retry later.
    Full,
    /// The reply channel was dropped before a response was sent.
    Recv,
}

impl<T> From<mpsc::error::TrySendError<T>> for AppRequestError {
    fn from(err: mpsc::error::TrySendError<T>) -> Self {
        match err {
            mpsc::error::TrySendError::Closed(_) => AppRequestError::Closed,
            mpsc::error::TrySendError::Full(_) => AppRequestError::Full,
        }
    }
}

impl From<oneshot::error::RecvError> for AppRequestError {
    fn from(_: oneshot::error::RecvError) -> Self {
        AppRequestError::Recv
    }
}

#[derive(Debug)]
pub struct Status {
    pub height: Height,
    pub round: Round,
    pub address: Address,
    pub public_key: PublicKey,
    pub proposer: Option<Address>,
    pub height_start_time: SystemTime,
    pub prev_payload_hash: Option<BlockHash>,
    pub db_latest_height: Height,
    pub db_earliest_height: Height,
    pub undecided_blocks_count: usize,
    pub pending_proposal_parts: Vec<(Height, usize)>,
    pub validator_set: ValidatorSet,
    pub sync_state: SyncState,
}

#[derive(Debug)]
pub struct CommitCertificateInfo {
    pub certificate: CommitCertificate<ArcContext>,
    pub certificate_type: CommitCertificateType,
    pub proposer: Address,
}

/// Forward range of heights `from..=from+count-1`.
///
/// `count` is validated against the RPC cap before the request is sent.
#[derive(Debug, PartialEq, Eq)]
pub struct HeightRangeRequest {
    /// First height of the range.
    pub from: Height,
    /// Total number of heights, including `from`.
    pub count: u64,
}

/// A bounded invalid-payloads query over heights `from..=from+count-1`.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidPayloadsRequest {
    /// First height of the range.
    pub from: Height,
    /// Total number of heights, including `from`.
    pub count: u64,
}

#[allow(clippy::enum_variant_names)]
pub enum AppRequest {
    /// Retrieves a commit certificate at the given height
    GetCertificate {
        /// The height to get the certificate for. If None, get the latest certificate.
        height: Option<Height>,
        /// The time the request was enqueued.
        enqueued_at: Instant,
        /// The channel to send the certificate back on.
        reply: oneshot::Sender<Option<CommitCertificateInfo>>,
    },
    /// Retrieves misbehavior evidence at the given height
    GetMisbehaviorEvidence {
        height: Option<Height>,
        enqueued_at: Instant,
        reply: oneshot::Sender<Option<StoredMisbehaviorEvidence>>,
    },
    /// Retrieves proposal monitor data at the given height
    GetProposalMonitorData {
        height: Option<Height>,
        enqueued_at: Instant,
        reply: oneshot::Sender<Option<ProposalMonitor>>,
    },
    /// Retrieves invalid payloads at the given height
    GetInvalidPayloads {
        height: Option<Height>,
        enqueued_at: Instant,
        reply: oneshot::Sender<Option<StoredInvalidPayloads>>,
    },
    /// Retrieves a bounded page of commit certificates for a range of heights
    GetCertificateRange {
        /// The range of heights to read.
        range: HeightRangeRequest,
        /// The time the request was enqueued.
        enqueued_at: Instant,
        /// The channel to send the certificates back on.
        reply: oneshot::Sender<Option<HeightRangePage<CommitCertificateInfo>>>,
    },
    /// Retrieves a bounded page of misbehavior evidence for a range of heights
    GetMisbehaviorEvidenceRange {
        /// The range of heights to read.
        range: HeightRangeRequest,
        /// The time the request was enqueued.
        enqueued_at: Instant,
        /// The channel to send the evidence back on.
        reply: oneshot::Sender<Option<HeightRangePage<StoredMisbehaviorEvidence>>>,
    },
    /// Retrieves a bounded page of proposal monitor data for a range of heights
    GetProposalMonitorDataRange {
        /// The range of heights to read.
        range: HeightRangeRequest,
        /// The time the request was enqueued.
        enqueued_at: Instant,
        /// The channel to send the data back on.
        reply: oneshot::Sender<Option<HeightRangePage<ProposalMonitor>>>,
    },
    /// Retrieves a bounded page of invalid payloads for a range of heights
    GetInvalidPayloadsRange {
        /// The query to serve.
        request: InvalidPayloadsRequest,
        /// The time the request was enqueued.
        enqueued_at: Instant,
        /// The channel to send the payloads back on.
        reply: oneshot::Sender<Option<HeightRangePage<StoredInvalidPayloads>>>,
    },
    /// Get the application status
    GetStatus {
        enqueued_at: Instant,
        reply: oneshot::Sender<Status>,
    },
    /// Check if the application is healthy
    GetHealth {
        enqueued_at: Instant,
        reply: oneshot::Sender<Health>,
    },
    /// Get the current sync state (lightweight, no DB queries)
    GetSyncState {
        enqueued_at: Instant,
        reply: oneshot::Sender<SyncState>,
    },
}

impl AppRequest {
    /// Whether this request is a liveness/readiness probe that must not wait
    /// behind a store range query.
    pub(crate) fn is_liveness_probe(&self) -> bool {
        matches!(self, Self::GetHealth { .. } | Self::GetSyncState { .. })
    }

    /// Whether the RPC caller has already dropped the reply receiver.
    ///
    /// A dropped receiver means the HTTP handler timed out. Store and
    /// execution-layer work for this request would be thrown away.
    pub(crate) fn reply_is_closed(&self) -> bool {
        match self {
            Self::GetCertificate { reply, .. } => reply.is_closed(),
            Self::GetMisbehaviorEvidence { reply, .. } => reply.is_closed(),
            Self::GetProposalMonitorData { reply, .. } => reply.is_closed(),
            Self::GetInvalidPayloads { reply, .. } => reply.is_closed(),
            Self::GetCertificateRange { reply, .. } => reply.is_closed(),
            Self::GetMisbehaviorEvidenceRange { reply, .. } => reply.is_closed(),
            Self::GetProposalMonitorDataRange { reply, .. } => reply.is_closed(),
            Self::GetInvalidPayloadsRange { reply, .. } => reply.is_closed(),
            Self::GetStatus { reply, .. } => reply.is_closed(),
            Self::GetHealth { reply, .. } => reply.is_closed(),
            Self::GetSyncState { reply, .. } => reply.is_closed(),
        }
    }

    /// Resolves when the RPC caller drops the reply receiver.
    pub(crate) async fn reply_closed(&mut self) {
        match self {
            Self::GetCertificate { reply, .. } => reply.closed().await,
            Self::GetMisbehaviorEvidence { reply, .. } => reply.closed().await,
            Self::GetProposalMonitorData { reply, .. } => reply.closed().await,
            Self::GetInvalidPayloads { reply, .. } => reply.closed().await,
            Self::GetCertificateRange { reply, .. } => reply.closed().await,
            Self::GetMisbehaviorEvidenceRange { reply, .. } => reply.closed().await,
            Self::GetProposalMonitorDataRange { reply, .. } => reply.closed().await,
            Self::GetInvalidPayloadsRange { reply, .. } => reply.closed().await,
            Self::GetStatus { reply, .. } => reply.closed().await,
            Self::GetHealth { reply, .. } => reply.closed().await,
            Self::GetSyncState { reply, .. } => reply.closed().await,
        }
    }
}

/// Result of a `/health` probe.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// The consensus event loop has progressed recently.
    Ok,
    /// The `go()` loop has not been able to poll for longer than
    /// [`CONSENSUS_LOOP_LIVENESS`]. It is likely blocked inside
    /// `handle_consensus`.
    EventLoopStalled,
}

/// Age beyond which `/health` treats the consensus `go()` loop as wedged.
///
/// Longer than one Engine API call (8s) and a typical propose sequence so a
/// live loop blocked on EL work stays healthy. Short enough that a deadlock
/// in `handle_consensus` fails k8s liveness.
pub const CONSENSUS_LOOP_LIVENESS: Duration = Duration::from_secs(60);

/// Whether a heartbeat of this age still counts as a live consensus loop.
pub fn consensus_loop_is_live(age: Duration) -> bool {
    age <= CONSENSUS_LOOP_LIVENESS
}

/// Map the consensus `go()` heartbeat to the `/health` result.
///
/// `None` means `ConsensusReady` has not completed. Startup work runs inline on
/// the `go()` loop and may exceed [`CONSENSUS_LOOP_LIVENESS`], so it reports OK.
pub fn health_from_heartbeat(
    heartbeat_rx: &watch::Receiver<Option<tokio::time::Instant>>,
) -> Health {
    match *heartbeat_rx.borrow() {
        Some(heartbeat) if !consensus_loop_is_live(heartbeat.elapsed()) => Health::EventLoopStalled,
        _ => Health::Ok,
    }
}

pub type TxAppReq = mpsc::Sender<AppRequest>;

impl AppRequest {
    /// Prometheus `request` label for a single-height certificate query.
    pub const LABEL_GET_CERTIFICATE: &'static str = "get_certificate";
    /// Prometheus `request` label for a certificate range query.
    pub const LABEL_GET_CERTIFICATE_RANGE: &'static str = "get_certificate_range";
    /// Prometheus `request` label for a single-height misbehavior query.
    pub const LABEL_GET_MISBEHAVIOR_EVIDENCE: &'static str = "get_misbehavior_evidence";
    /// Prometheus `request` label for a misbehavior range query.
    pub const LABEL_GET_MISBEHAVIOR_EVIDENCE_RANGE: &'static str = "get_misbehavior_evidence_range";
    /// Prometheus `request` label for a single-height proposal-monitor query.
    pub const LABEL_GET_PROPOSAL_MONITOR_DATA: &'static str = "get_proposal_monitor_data";
    /// Prometheus `request` label for a proposal-monitor range query.
    pub const LABEL_GET_PROPOSAL_MONITOR_DATA_RANGE: &'static str =
        "get_proposal_monitor_data_range";
    /// Prometheus `request` label for a single-height invalid-payloads query.
    pub const LABEL_GET_INVALID_PAYLOADS: &'static str = "get_invalid_payloads";
    /// Prometheus `request` label for an invalid-payloads range query.
    pub const LABEL_GET_INVALID_PAYLOADS_RANGE: &'static str = "get_invalid_payloads_range";
    /// Prometheus `request` label for status.
    pub const LABEL_GET_STATUS: &'static str = "get_status";
    /// Prometheus `request` label for health.
    pub const LABEL_GET_HEALTH: &'static str = "get_health";
    /// Prometheus `request` label for sync state.
    pub const LABEL_GET_SYNC_STATE: &'static str = "get_sync_state";

    /// Stable Prometheus `request` label for this variant.
    ///
    /// Values are snake_case so they match Prometheus label naming
    /// (`operation="create|update|delete"`).
    pub fn metric_label(&self) -> &'static str {
        match self {
            Self::GetCertificate { .. } => Self::LABEL_GET_CERTIFICATE,
            Self::GetMisbehaviorEvidence { .. } => Self::LABEL_GET_MISBEHAVIOR_EVIDENCE,
            Self::GetProposalMonitorData { .. } => Self::LABEL_GET_PROPOSAL_MONITOR_DATA,
            Self::GetInvalidPayloads { .. } => Self::LABEL_GET_INVALID_PAYLOADS,
            Self::GetCertificateRange { .. } => Self::LABEL_GET_CERTIFICATE_RANGE,
            Self::GetMisbehaviorEvidenceRange { .. } => Self::LABEL_GET_MISBEHAVIOR_EVIDENCE_RANGE,
            Self::GetProposalMonitorDataRange { .. } => Self::LABEL_GET_PROPOSAL_MONITOR_DATA_RANGE,
            Self::GetInvalidPayloadsRange { .. } => Self::LABEL_GET_INVALID_PAYLOADS_RANGE,
            Self::GetStatus { .. } => Self::LABEL_GET_STATUS,
            Self::GetHealth { .. } => Self::LABEL_GET_HEALTH,
            Self::GetSyncState { .. } => Self::LABEL_GET_SYNC_STATE,
        }
    }

    /// When this request was placed on the app-request channel.
    pub fn enqueued_at(&self) -> Instant {
        match self {
            Self::GetCertificate { enqueued_at, .. }
            | Self::GetMisbehaviorEvidence { enqueued_at, .. }
            | Self::GetProposalMonitorData { enqueued_at, .. }
            | Self::GetInvalidPayloads { enqueued_at, .. }
            | Self::GetCertificateRange { enqueued_at, .. }
            | Self::GetMisbehaviorEvidenceRange { enqueued_at, .. }
            | Self::GetProposalMonitorDataRange { enqueued_at, .. }
            | Self::GetInvalidPayloadsRange { enqueued_at, .. }
            | Self::GetStatus { enqueued_at, .. }
            | Self::GetHealth { enqueued_at, .. }
            | Self::GetSyncState { enqueued_at, .. } => *enqueued_at,
        }
    }

    /// Request the certificate for a height.
    ///
    /// If the request fails, return an error.
    pub async fn get_certificate(
        height: Option<Height>,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<CommitCertificateInfo>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetCertificate {
                height,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| error!("Failed to send GetCertificate request to consensus: {e}"))?;

        let cert = rx.await.inspect_err(|e| {
            error!("Failed to receive GetCertificate response from consensus: {e}")
        })?;

        Ok(cert)
    }

    /// Request the misbehavior evidence for a height.
    ///
    /// Returns:
    /// - `Some(evidence)` with actual or empty evidence for finalized heights
    /// - `None` if the requested height was not yet finalized
    pub async fn get_misbehavior_evidence(
        height: Option<Height>,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<StoredMisbehaviorEvidence>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetMisbehaviorEvidence {
                height,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetMisbehaviorEvidence request to consensus: {e}")
            })?;

        let evidence = rx.await.inspect_err(|e| {
            error!("Failed to receive GetMisbehaviorEvidence response from consensus: {e}")
        })?;

        Ok(evidence)
    }

    /// Request the proposal monitor data for a height.
    ///
    /// If the request fails, return `None`.
    pub async fn get_proposal_monitor_data(
        height: Option<Height>,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<ProposalMonitor>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetProposalMonitorData {
                height,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetProposalMonitorData request to consensus: {e}")
            })?;

        let data = rx.await.inspect_err(|e| {
            error!("Failed to receive GetProposalMonitorData response from consensus: {e}")
        })?;

        Ok(data)
    }

    /// Request the invalid payloads for a height.
    ///
    /// Returns:
    /// - `Some(payloads)` with actual or empty payloads for finalized heights
    /// - `None` if the requested height was not yet finalized
    pub async fn get_invalid_payloads(
        height: Option<Height>,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<StoredInvalidPayloads>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetInvalidPayloads {
                height,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetInvalidPayloads request to consensus: {e}")
            })?;

        let payloads = rx.await.inspect_err(|e| {
            error!("Failed to receive GetInvalidPayloads response from consensus: {e}")
        })?;

        Ok(payloads)
    }

    /// Request commit certificates for a range of heights.
    ///
    /// Returns `Ok(None)` when the store has no certificates at all.
    pub async fn get_certificate_range(
        range: HeightRangeRequest,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<HeightRangePage<CommitCertificateInfo>>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetCertificateRange {
                range,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetCertificateRange request to consensus: {e}")
            })?;

        let certs = rx.await.inspect_err(|e| {
            error!("Failed to receive GetCertificateRange response from consensus: {e}")
        })?;

        Ok(certs)
    }

    /// Request misbehavior evidence for a range of heights.
    ///
    /// Returns `Ok(None)` when the store has no certificates at all.
    pub async fn get_misbehavior_evidence_range(
        range: HeightRangeRequest,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<HeightRangePage<StoredMisbehaviorEvidence>>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetMisbehaviorEvidenceRange {
                range,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetMisbehaviorEvidenceRange request to consensus: {e}")
            })?;

        let evidence = rx.await.inspect_err(|e| {
            error!("Failed to receive GetMisbehaviorEvidenceRange response from consensus: {e}")
        })?;

        Ok(evidence)
    }

    /// Request proposal monitor data for a range of heights.
    ///
    /// Returns `Ok(None)` when the store has no certificates at all.
    pub async fn get_proposal_monitor_data_range(
        range: HeightRangeRequest,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<HeightRangePage<ProposalMonitor>>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetProposalMonitorDataRange {
                range,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetProposalMonitorDataRange request to consensus: {e}")
            })?;

        let data = rx.await.inspect_err(|e| {
            error!("Failed to receive GetProposalMonitorDataRange response from consensus: {e}")
        })?;

        Ok(data)
    }

    /// Request a bounded page of invalid payloads for a range of heights.
    ///
    /// Returns `Ok(None)` when the store has no certificates at all.
    pub async fn get_invalid_payloads_range(
        request: InvalidPayloadsRequest,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<HeightRangePage<StoredInvalidPayloads>>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetInvalidPayloadsRange {
                request,
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| {
                error!("Failed to send GetInvalidPayloadsRange request to consensus: {e}")
            })?;

        let payloads = rx.await.inspect_err(|e| {
            error!("Failed to receive GetInvalidPayloadsRange response from consensus: {e}")
        })?;

        Ok(payloads)
    }

    /// Get `malachite-app`'s status.
    ///
    /// If the request fails, return an error.
    pub async fn get_status(
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Status, AppRequestError> {
        let (tx, rx) = oneshot::channel();

        tx_app_req
            .try_send(Self::GetStatus {
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| error!("Failed to send GetStatus request to consensus: {e}"))?;

        let status = rx
            .await
            .inspect_err(|e| error!("Failed to receive GetStatus response from consensus: {e}"))?;

        Ok(status)
    }

    /// Get the current sync state. Lightweight — reads an in-memory field, no DB queries.
    pub async fn get_sync_state(
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<SyncState, AppRequestError> {
        let (tx, rx) = oneshot::channel();

        tx_app_req
            .try_send(Self::GetSyncState {
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| error!("Failed to send GetSyncState request to consensus: {e}"))?;

        let sync_state = rx.await.inspect_err(|e| {
            error!("Failed to receive GetSyncState response from consensus: {e}")
        })?;

        Ok(sync_state)
    }

    /// Get node's health, including whether the consensus event loop is progressing.
    ///
    /// If the request fails, return an error.
    pub async fn get_health(
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Health, AppRequestError> {
        let (tx, rx) = oneshot::channel();

        tx_app_req
            .try_send(Self::GetHealth {
                enqueued_at: Instant::now(),
                reply: tx,
            })
            .inspect_err(|e| error!("Failed to send GetHealth request to consensus: {e}"))?;

        let status = rx
            .await
            .inspect_err(|e| error!("Failed to receive GetHealth response from consensus: {e}"))?;

        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    fn all_variants(enqueued_at: Instant) -> Vec<AppRequest> {
        let range = HeightRangeRequest {
            from: Height::new(1),
            count: 2,
        };
        let invalid = InvalidPayloadsRequest {
            from: Height::new(1),
            count: 2,
        };
        vec![
            AppRequest::GetCertificate {
                height: None,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetMisbehaviorEvidence {
                height: None,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetProposalMonitorData {
                height: None,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetInvalidPayloads {
                height: None,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetCertificateRange {
                range,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetMisbehaviorEvidenceRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetProposalMonitorDataRange {
                range: HeightRangeRequest {
                    from: Height::new(1),
                    count: 2,
                },
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetInvalidPayloadsRange {
                request: invalid,
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetStatus {
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetHealth {
                enqueued_at,
                reply: oneshot::channel().0,
            },
            AppRequest::GetSyncState {
                enqueued_at,
                reply: oneshot::channel().0,
            },
        ]
    }

    #[test]
    fn app_request_metric_labels_are_snake_case() {
        let labels = [
            AppRequest::LABEL_GET_CERTIFICATE,
            AppRequest::LABEL_GET_CERTIFICATE_RANGE,
            AppRequest::LABEL_GET_MISBEHAVIOR_EVIDENCE,
            AppRequest::LABEL_GET_MISBEHAVIOR_EVIDENCE_RANGE,
            AppRequest::LABEL_GET_PROPOSAL_MONITOR_DATA,
            AppRequest::LABEL_GET_PROPOSAL_MONITOR_DATA_RANGE,
            AppRequest::LABEL_GET_INVALID_PAYLOADS,
            AppRequest::LABEL_GET_INVALID_PAYLOADS_RANGE,
            AppRequest::LABEL_GET_STATUS,
            AppRequest::LABEL_GET_HEALTH,
            AppRequest::LABEL_GET_SYNC_STATE,
        ];
        for label in labels {
            assert!(
                label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "Prometheus request labels must be snake_case, got {label}"
            );
            assert!(
                !label.is_empty() && !label.starts_with('_') && !label.ends_with('_'),
                "label must not be empty or underscored at the edges: {label}"
            );
        }
    }

    #[test]
    fn metric_label_and_enqueued_at_cover_every_variant() {
        let enqueued_at = Instant::now() - Duration::from_millis(7);
        let expected = [
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
        ];
        let variants = all_variants(enqueued_at);
        assert_eq!(variants.len(), expected.len());
        for (req, label) in variants.iter().zip(expected) {
            assert_eq!(req.metric_label(), label);
            assert_eq!(req.enqueued_at(), enqueued_at);
        }
    }
    use super::*;
    use tokio::sync::oneshot;

    #[test]
    fn liveness_probes_are_health_and_sync_state() {
        let (tx, _rx) = oneshot::channel();
        assert!(AppRequest::GetHealth {
            enqueued_at: Instant::now(),
            reply: tx,
        }
        .is_liveness_probe());
        let (tx, _rx) = oneshot::channel();
        assert!(AppRequest::GetSyncState {
            enqueued_at: Instant::now(),
            reply: tx,
        }
        .is_liveness_probe());
    }

    #[test]
    fn range_queries_are_not_liveness_probes() {
        let (tx, _rx) = oneshot::channel();
        let range = HeightRangeRequest {
            from: Height::new(1),
            count: 10,
        };
        assert!(!AppRequest::GetCertificateRange {
            range,
            enqueued_at: Instant::now(),
            reply: tx,
        }
        .is_liveness_probe());
    }

    #[test]
    fn status_is_not_a_liveness_probe() {
        let (tx, _rx) = oneshot::channel();
        assert!(!AppRequest::GetStatus {
            enqueued_at: Instant::now(),
            reply: tx,
        }
        .is_liveness_probe());
    }

    #[tokio::test]
    async fn reply_closed_resolves_when_the_receiver_is_dropped() {
        let (tx, rx) = oneshot::channel::<Health>();
        let mut req = AppRequest::GetHealth {
            enqueued_at: Instant::now(),
            reply: tx,
        };
        assert!(!req.reply_is_closed());

        drop(rx);
        assert!(req.reply_is_closed());
        tokio::time::timeout(std::time::Duration::from_secs(1), req.reply_closed())
            .await
            .expect("a dropped receiver should wake reply_closed");
    }

    #[test]
    fn consensus_loop_is_live_within_budget() {
        assert!(consensus_loop_is_live(Duration::from_secs(0)));
        assert!(consensus_loop_is_live(Duration::from_secs(8)));
        assert!(consensus_loop_is_live(CONSENSUS_LOOP_LIVENESS));
    }

    #[test]
    fn consensus_loop_is_live_rejects_stale_heartbeat() {
        assert!(!consensus_loop_is_live(
            CONSENSUS_LOOP_LIVENESS + Duration::from_millis(1)
        ));
        assert!(!consensus_loop_is_live(Duration::from_secs(61)));
    }

    #[test]
    fn health_from_heartbeat_ok_when_fresh() {
        let (_tx, rx) = watch::channel(Some(tokio::time::Instant::now()));
        assert_eq!(health_from_heartbeat(&rx), Health::Ok);
    }

    #[test]
    fn health_from_heartbeat_stalled_when_stale() {
        let stale = tokio::time::Instant::now()
            .checked_sub(CONSENSUS_LOOP_LIVENESS + Duration::from_millis(1))
            .expect("monotonic clock has more than 60s of history");
        let (_tx, rx) = watch::channel(Some(stale));
        assert_eq!(health_from_heartbeat(&rx), Health::EventLoopStalled);
    }

    #[test]
    fn health_from_heartbeat_ok_before_startup_completes() {
        let (_tx, rx) = watch::channel(None);
        assert_eq!(health_from_heartbeat(&rx), Health::Ok);
    }
}
