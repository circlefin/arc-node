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

use eyre::{bail, eyre};
use tracing::{error, info, warn};

use malachitebft_app_channel::app::engine::host::Next;
use malachitebft_app_channel::app::types::MisbehaviorEvidence;
use malachitebft_app_channel::Reply;
use malachitebft_core_types::{CommitCertificate, HeightParams};

use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::{ArcContext, Height};

use crate::state::{Decision, NextHeightInfo, State};
use crate::utils::check_halt_height;

/// Handles the `Finalized` message from the consensus engine.
///
/// This message always follows a corresponding a `Decided` message, whose processing defines
/// the `Next` message to be sent to consensus, in order to start a new or restart a height.
///
/// This method sends the appropriate `Next` message to consensus to start the next height.
#[tracing::instrument(
    name = "finalized",
    skip_all,
    fields(
        height = %certificate.height,
        round = %certificate.round,
    ))]
pub async fn handle(
    state: &mut State,
    certificate: CommitCertificate<ArcContext>,
    evidence: MisbehaviorEvidence<ArcContext>,
    reply: Reply<Next<ArcContext>>,
) -> eyre::Result<()> {
    let Some(decision) = state.decision.take() else {
        bail!("Finalized: Received message without a corresponding decision in state");
    };

    // Store misbehavior evidence if any was observed during this height
    if !evidence.is_empty() {
        let stored_evidence =
            StoredMisbehaviorEvidence::from_misbehavior_evidence(certificate.height, &evidence);

        let validators_count = stored_evidence.validators.len();

        if let Err(e) = state
            .store()
            .store_misbehavior_evidence(stored_evidence)
            .await
        {
            warn!("Failed to store misbehavior evidence: {e:#}");
        } else {
            info!(%validators_count, "Stored misbehavior evidence");
        }
    }

    let height = certificate.height;
    let signatures_count = certificate.commit_signatures.len();

    if let Err(e) = state.store().extend_certificate(certificate).await {
        error!(
            %signatures_count,
            "Failed to store extended certificate: {e}"
        );

        // Continue anyway - shouldn't block consensus progress
    }

    let next = match decision {
        Decision::Success(next_height_info) => start_next_height(state, *next_height_info).await?,
        Decision::Failure(report) => {
            error!(error = ?report, "🔴 Decision failure, restarting height");

            restart_height(state, height).await?
        }
    };

    info!(
        duration = ?state.stats().height_started().elapsed(),
        "Height duration from started until finalized"
    );

    reply
        .send(next)
        .map_err(|e| eyre!("Finalized: Failed to send Next reply for height {height}: {e:?}"))?;

    Ok(())
}

/// Prepare the start of the next height after a successful decision.
async fn start_next_height(
    state: &mut State,
    info: NextHeightInfo,
) -> eyre::Result<Next<ArcContext>> {
    let next_height = info.next_height;
    let next_params = info.height_params();

    state.move_to_next_height(info);

    // Check if the next height matches the configured halt height (if any).
    // If so, the node will halt instead of starting the next height.
    let halt_height = state.env_config().halt_height;
    check_halt_height(state.store(), next_height, halt_height).await?;

    Ok(Next::Start(next_height, next_params))
}

/// Prepare the restart of the current height after a failed decision.
async fn restart_height(state: &mut State, height: Height) -> eyre::Result<Next<ArcContext>> {
    let validator_set = state.validator_set().clone();
    let consensus_params = state.consensus_params().clone();
    let timeouts = consensus_params.timeouts();

    state
        .restart_height(height, validator_set.clone(), consensus_params)
        .await?;

    // Note that no `target_time` is set for a height that is restarted
    let height_params = HeightParams::new(validator_set, timeouts, None);
    Ok(Next::Restart(height, height_params))
}
