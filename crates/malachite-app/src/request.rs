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

use std::time::SystemTime;

use tokio::sync::{mpsc, oneshot};
use tracing::error;

use arc_consensus_db::invalid_payloads::StoredInvalidPayloads;
use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::{
    Address, ArcContext, BlockHash, CommitCertificateType, Height, Round, ValidatorSet,
};
use malachitebft_core_types::CommitCertificate;

use arc_consensus_types::proposal_monitor::ProposalMonitor;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AppRequestError {
    Closed,
    Full,
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
    pub proposer: Option<Address>,
    pub height_start_time: SystemTime,
    pub prev_payload_hash: Option<BlockHash>,
    pub db_latest_height: Height,
    pub db_earliest_height: Height,
    pub undecided_blocks_count: usize,
    pub pending_proposal_parts: Vec<(Height, usize)>,
    pub validator_set: ValidatorSet,
}

#[derive(Debug)]
pub struct CommitCertificateInfo {
    pub certificate: CommitCertificate<ArcContext>,
    pub certificate_type: CommitCertificateType,
    pub proposer: Address,
}

#[allow(clippy::enum_variant_names)]
pub enum AppRequest {
    /// Retrieves a commit certificate at the given height
    GetCertificate(
        /// The height to get the certificate for. If None, get the latest certificate.
        Option<Height>,
        /// The channel to send the certificate back on.
        oneshot::Sender<Option<CommitCertificateInfo>>,
    ),
    /// Retrieves misbehavior evidence at the given height
    GetMisbehaviorEvidence(
        /// The height to get the evidence for. If None, use the latest height.
        Option<Height>,
        /// The channel to send the evidence back on.
        oneshot::Sender<Option<StoredMisbehaviorEvidence>>,
    ),
    /// Retrieves proposal monitor data at the given height
    GetProposalMonitorData(
        /// The height to get the data for. If None, get the latest.
        Option<Height>,
        /// The channel to send the data back on.
        oneshot::Sender<Option<ProposalMonitor>>,
    ),
    /// Retrieves invalid payloads at the given height
    GetInvalidPayloads(
        /// The height to get the payloads for. If None, get the latest.
        Option<Height>,
        /// The channel to send the payloads back on.
        oneshot::Sender<Option<StoredInvalidPayloads>>,
    ),
    /// Get the application status
    GetStatus(oneshot::Sender<Status>),
    /// Check if the application is healthy
    GetHealth(oneshot::Sender<()>),
}

pub type TxAppReq = mpsc::Sender<AppRequest>;

impl AppRequest {
    /// Request the certificate for a height.
    ///
    /// If the request fails, return an error.
    pub async fn get_certificate(
        height: Option<Height>,
        tx_app_req: &mpsc::Sender<AppRequest>,
    ) -> Result<Option<CommitCertificateInfo>, AppRequestError> {
        let (tx, rx) = oneshot::channel();
        tx_app_req
            .try_send(Self::GetCertificate(height, tx))
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
            .try_send(Self::GetMisbehaviorEvidence(height, tx))
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
            .try_send(Self::GetProposalMonitorData(height, tx))
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
            .try_send(Self::GetInvalidPayloads(height, tx))
            .inspect_err(|e| {
                error!("Failed to send GetInvalidPayloads request to consensus: {e}")
            })?;

        let payloads = rx.await.inspect_err(|e| {
            error!("Failed to receive GetInvalidPayloads response from consensus: {e}")
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
            .try_send(Self::GetStatus(tx))
            .inspect_err(|e| error!("Failed to send GetStatus request to consensus: {e}"))?;

        let status = rx
            .await
            .inspect_err(|e| error!("Failed to receive GetStatus response from consensus: {e}"))?;

        Ok(status)
    }

    /// Get node's health. Returns unit type. Used to check whether the node is responsive.
    ///
    /// If the request fails, return an error.
    pub async fn get_health(tx_app_req: &mpsc::Sender<AppRequest>) -> Result<(), AppRequestError> {
        let (tx, rx) = oneshot::channel();

        tx_app_req
            .try_send(Self::GetHealth(tx))
            .inspect_err(|e| error!("Failed to send GetHealth request to consensus: {e}"))?;

        #[allow(clippy::let_unit_value)]
        let status = rx
            .await
            .inspect_err(|e| error!("Failed to receive GetHealth response from consensus: {e}"))?;

        Ok(status)
    }
}
