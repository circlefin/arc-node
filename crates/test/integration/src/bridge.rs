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

//! Event bridge: maps consensus [`Event<ArcContext>`] into [`ArcEvent`].
//!
//! The bridge subscribes to the consensus engine's [`TxEvent`] and forwards
//! mapped events to a `broadcast::Sender<ArcEvent>` consumed by the step
//! sequencer.

use alloy_primitives::B256;
use arc_consensus_types::ArcContext;
use arc_test_framework::events::ArcEvent;
use malachitebft_app_channel::app::engine::util::events::{Event, TxEvent};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Map a consensus [`Event<ArcContext>`] to an [`ArcEvent`], if applicable.
///
/// Returns `None` for internal consensus events that the test framework
/// does not need (e.g. round starts, WAL replays, vote broadcasts).
fn map_event(event: Event<ArcContext>) -> Option<ArcEvent> {
    match event {
        Event::StartedHeight(height, false) => Some(ArcEvent::ConsensusStartedHeight { height }),
        Event::Decided { commit_certificate } => {
            let height = commit_certificate.height;
            Some(ArcEvent::ConsensusDecided {
                height,
                certificate: commit_certificate,
            })
        }
        Event::Finalized {
            commit_certificate, ..
        } => Some(ArcEvent::ConsensusFinalized {
            height: commit_certificate.height,
        }),
        Event::ProposedValue(proposed) => Some(ArcEvent::ConsensusProposedValue {
            height: proposed.height,
            round: proposed.round,
        }),
        _ => None,
    }
}

/// Derive a [`ArcEvent::BlockProduced`] from a decided height.
///
/// Until we hook into Reth's block notification system, we synthesize
/// `BlockProduced` from `ConsensusDecided` with a zero hash.
fn block_produced_from_decided(height: arc_consensus_types::Height) -> ArcEvent {
    ArcEvent::BlockProduced {
        number: height,
        hash: B256::ZERO,
    }
}

/// Spawn a background task that bridges consensus events to the framework's
/// unified [`ArcEvent`] broadcast channel.
///
/// The task runs until the consensus event channel closes (node shutdown).
pub fn spawn_event_bridge(
    tx_event: TxEvent<ArcContext>,
    tx: broadcast::Sender<ArcEvent>,
) -> JoinHandle<()> {
    let mut rx = tx_event.subscribe();

    tokio::spawn(async move {
        loop {
            let event = match rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        lagged = n,
                        "event bridge: receiver lagged, some events dropped"
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("event bridge: consensus event channel closed");
                    return;
                }
            };

            let Some(arc_event) = map_event(event) else {
                continue;
            };

            let decided_height = match &arc_event {
                ArcEvent::ConsensusDecided { height, .. } => Some(*height),
                _ => None,
            };

            if tx.send(arc_event).is_err() {
                debug!("event bridge: no receivers for consensus event");
            }

            if let Some(h) = decided_height {
                if tx.send(block_produced_from_decided(h)).is_err() {
                    debug!(height = %h, "event bridge: no receivers for synthesized block event");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use arc_consensus_types::Height;

    use super::*;

    async fn recv_started_height(rx: &mut broadcast::Receiver<ArcEvent>, expected: Height) {
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("event timed out")
            .expect("event channel closed");

        assert!(matches!(
            event,
            ArcEvent::ConsensusStartedHeight { height } if height == expected
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribes_to_consensus_events_before_returning() {
        let tx_event = TxEvent::<ArcContext>::new();
        let (tx, _) = broadcast::channel::<ArcEvent>(16);
        let mut rx = tx.subscribe();

        let bridge_task = spawn_event_bridge(tx_event.clone(), tx);
        tx_event.send(|| Event::StartedHeight(Height::new(3), false));

        recv_started_height(&mut rx, Height::new(3)).await;

        bridge_task.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keeps_running_when_arc_event_bus_has_no_receivers() {
        let tx_event = TxEvent::<ArcContext>::new();
        let (tx, _) = broadcast::channel::<ArcEvent>(16);

        let bridge_task = spawn_event_bridge(tx_event.clone(), tx.clone());
        tokio::task::yield_now().await;

        tx_event.send(|| Event::StartedHeight(Height::new(1), false));
        tokio::task::yield_now().await;

        let mut rx = tx.subscribe();
        tx_event.send(|| Event::StartedHeight(Height::new(2), false));

        recv_started_height(&mut rx, Height::new(2)).await;

        bridge_task.abort();
    }
}
