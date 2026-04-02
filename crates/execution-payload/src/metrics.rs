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

//! Prometheus metrics for payload building.
//!
//! Stage durations use a single histogram family with a `stage` label:
//!   `arc_payload_stage_duration_seconds{stage="state_setup|pre_execution|tx_execution|post_execution|assembly_and_sealing"}`
//!
//! Total build duration:
//!   `arc_payload_total_duration_seconds`
//!
//! Outcome counters use a single counter family with an `outcome` label:
//!   `arc_payload_build_outcome_total{outcome="better|aborted|cancelled"}`
//!
//! Outcome semantics:
//! - **better**: built payload had higher total fees than current best; payload was sealed and returned.
//! - **aborted**: built candidate was not better than current best (by fees); candidate was discarded.
//! - **cancelled**: build was cancelled (e.g. job cancelled) before completion.

use std::time::{Duration, Instant};

/// Metric name constants.
const STAGE_DURATION: &str = "arc_payload_stage_duration_seconds";
const TOTAL_DURATION: &str = "arc_payload_total_duration_seconds";
const BUILD_OUTCOME: &str = "arc_payload_build_outcome_total";

/// Label keys.
const STAGE_LABEL: &str = "stage";
const OUTCOME_LABEL: &str = "outcome";

/// Stage label values.
const STATE_SETUP_STAGE: &str = "state_setup";
const PRE_EXECUTION_STAGE: &str = "pre_execution";
const TX_EXECUTION_STAGE: &str = "tx_execution";
const POST_EXECUTION_STAGE: &str = "post_execution";
const ASSEMBLY_AND_SEALING_STAGE: &str = "assembly_and_sealing";

/// Outcome label values.
const BETTER_OUTCOME: &str = "better";
const ABORTED_OUTCOME: &str = "aborted";
const CANCELLED_OUTCOME: &str = "cancelled";

/// Records payload build metrics to Prometheus.
///
/// Stage durations use a single histogram family with a `stage` label:
///   `arc_payload_stage_duration_seconds{stage="..."}`
///
/// Total build duration:
///   `arc_payload_total_duration_seconds`
///
/// Outcome counters use a single counter family with an `outcome` label:
///   `arc_payload_build_outcome_total{outcome="..."}`
#[derive(Debug, Clone)]
pub struct PayloadBuildMetrics;

impl PayloadBuildMetrics {
    // ── Stage durations (histograms) ──

    pub fn record_stage_state_setup(duration: Duration) {
        metrics::histogram!(STAGE_DURATION, STAGE_LABEL => STATE_SETUP_STAGE)
            .record(duration.as_secs_f64());
    }

    pub fn record_stage_pre_execution(duration: Duration) {
        metrics::histogram!(STAGE_DURATION, STAGE_LABEL => PRE_EXECUTION_STAGE)
            .record(duration.as_secs_f64());
    }

    pub fn record_stage_tx_execution(duration: Duration) {
        metrics::histogram!(STAGE_DURATION, STAGE_LABEL => TX_EXECUTION_STAGE)
            .record(duration.as_secs_f64());
    }

    /// Records the post-execution stage, which covers `builder.finish()` —
    /// primarily state root computation and block finalization.
    pub fn record_stage_post_execution(duration: Duration) {
        metrics::histogram!(STAGE_DURATION, STAGE_LABEL => POST_EXECUTION_STAGE)
            .record(duration.as_secs_f64());
    }

    pub fn record_stage_assembly_and_sealing(duration: Duration) {
        metrics::histogram!(STAGE_DURATION, STAGE_LABEL => ASSEMBLY_AND_SEALING_STAGE)
            .record(duration.as_secs_f64());
    }

    // ── Total duration ──

    pub fn record_total_duration(payload_start: Instant) {
        metrics::histogram!(TOTAL_DURATION).record(payload_start.elapsed().as_secs_f64());
    }

    // ── Outcome counters ──

    pub fn record_outcome_better() {
        metrics::counter!(BUILD_OUTCOME, OUTCOME_LABEL => BETTER_OUTCOME).increment(1);
    }

    pub fn record_outcome_aborted() {
        metrics::counter!(BUILD_OUTCOME, OUTCOME_LABEL => ABORTED_OUTCOME).increment(1);
    }

    pub fn record_outcome_cancelled() {
        metrics::counter!(BUILD_OUTCOME, OUTCOME_LABEL => CANCELLED_OUTCOME).increment(1);
    }
}
