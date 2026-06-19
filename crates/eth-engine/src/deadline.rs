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

use std::time::Duration;
use tokio::time::Instant;

/// Leeway added to a budget-derived per-call timeout so its deadline lands
/// at least one timer tick beyond the outer budget deadline. Without it, a
/// per-call timeout equal to the remaining budget can tie the outer one
/// within tokio's ~1ms timer granularity, where firing order is undefined.
const CALL_TIMEOUT_LEEWAY: Duration = Duration::from_millis(20);

/// Wall-clock deadline for a budgeted sequence of Engine API calls.
///
/// The proposer derives this from the consensus round's propose budget
/// (which Malachite grows by `propose_delta` every round) so that
/// per-call Engine API timeouts never undercut the time consensus is
/// actually willing to wait. Sharing one deadline across a sequence of
/// calls means time spent in an earlier call (e.g. `forkchoice_updated`)
/// automatically shrinks what later calls (e.g. `get_payload`) may claim.
#[derive(Copy, Clone, Debug)]
pub struct EngineDeadline(Instant);

impl EngineDeadline {
    /// Deadline expiring `budget` from now.
    pub fn within(budget: Duration) -> Self {
        Self(
            Instant::now()
                .checked_add(budget)
                .expect("deadline must fit in Instant; consensus budgets are bounded"),
        )
    }

    /// Absolute deadline for the outer operation that owns this budget.
    pub fn timeout_at(self) -> Instant {
        self.0
    }

    /// Timeout for a single Engine API call: `floor`, or the remaining
    /// budget plus `leeway` when larger.
    ///
    /// Budgeted paths also wrap the sequence in an outer
    /// `timeout_at(deadline.timeout_at())` (see `on_get_value`). That outer
    /// timeout fails benignly (proposer skips the round, `Ok(None)`); a
    /// per-call timeout fails hard (aborts the consensus app), so it must
    /// stay strictly non-binding. `floor` guarantees that while the budget
    /// is below it; once the budget exceeds `floor`, `leeway` keeps the
    /// per-call deadline a timer tick past the outer one, avoiding a
    /// same-slot tie at tokio's ~1ms granularity. `floor` also bounds calls
    /// when no outer timeout is set.
    pub fn call_timeout(self, floor: Duration) -> Duration {
        let remaining = self.0.saturating_duration_since(Instant::now());
        floor.max(remaining.saturating_add(CALL_TIMEOUT_LEEWAY))
    }

    /// Resolve the timeout for a single Engine API call: `floor` when no
    /// deadline is active (unbudgeted paths), otherwise
    /// [`EngineDeadline::call_timeout`].
    pub fn resolve(deadline: Option<Self>, floor: Duration) -> Duration {
        deadline.map_or(floor, |d| d.call_timeout(floor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: Duration = Duration::from_secs(8);

    #[test]
    fn floor_wins_when_remaining_is_smaller() {
        let deadline = EngineDeadline::within(Duration::from_secs(1));
        assert_eq!(deadline.call_timeout(FLOOR), FLOOR);
    }

    #[test]
    fn remaining_wins_when_larger_than_floor() {
        let deadline = EngineDeadline::within(Duration::from_secs(20));
        let timeout = deadline.call_timeout(FLOOR);
        assert!(timeout > FLOOR);
        assert!(timeout <= Duration::from_secs(20).saturating_add(CALL_TIMEOUT_LEEWAY));
    }

    #[tokio::test(start_paused = true)]
    async fn adds_leeway_when_remaining_exceeds_floor() {
        // Paused clock: no time elapses between `within` and the call, so
        // `remaining` is exactly the budget and the leeway is observable.
        // A bare `max(floor, remaining)` would return exactly 20s here.
        let deadline = EngineDeadline::within(Duration::from_secs(20));
        assert_eq!(
            deadline.call_timeout(FLOOR),
            Duration::from_secs(20).saturating_add(CALL_TIMEOUT_LEEWAY),
        );
    }

    #[test]
    fn saturates_to_floor_when_budget_exhausted() {
        let deadline = EngineDeadline::within(Duration::ZERO);
        assert_eq!(deadline.call_timeout(FLOOR), FLOOR);
    }

    #[test]
    fn successive_calls_never_grow() {
        let deadline = EngineDeadline::within(Duration::from_secs(20));
        let first = deadline.call_timeout(FLOOR);
        let second = deadline.call_timeout(FLOOR);
        assert!(second <= first);
    }

    #[test]
    fn resolve_uses_floor_when_no_deadline() {
        assert_eq!(EngineDeadline::resolve(None, FLOOR), FLOOR);
    }

    #[test]
    fn resolve_extends_floor_with_active_deadline() {
        let deadline = EngineDeadline::within(Duration::from_secs(20));
        assert!(EngineDeadline::resolve(Some(deadline), FLOOR) > FLOOR);
    }
}
