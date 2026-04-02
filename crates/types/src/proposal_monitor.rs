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

//! Type for monitoring proposal timing and success.
//!
//! This module provides data structures to track:
//! - When a proposal was received relative to the height's start time
//! - Whether the recorded proposal was eventually finalized
//!
//! Data is recorded for both proposer and non-proposer nodes. For the proposer,
//! `proposal_delay_ms` roughly reflects the payload build time. For non-proposers,
//! it reflects network propagation delay.
//!
//! By design, monitoring data is only stored for round-0 proposals.

use std::time::SystemTime;
use tracing::warn;

use crate::{Address, Height, ValueId};

/// Indicates whether the proposal was finalized.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ProposalSuccessState {
    /// The success of the proposal is unknown (e.g., not recorded or incomplete data).
    Unknown = 0,
    /// The proposal was not successful (a different value was decided).
    Unsuccessful = 1,
    /// The proposal was successful (it was decided).
    Successful = 2,
}

impl ProposalSuccessState {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    pub fn is_unsuccessful(&self) -> bool {
        matches!(self, Self::Unsuccessful)
    }

    pub fn is_successful(&self) -> bool {
        matches!(self, Self::Successful)
    }

    pub fn to_bool(&self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Unsuccessful => Some(false),
            Self::Successful => Some(true),
        }
    }

    pub fn as_u32(&self) -> u32 {
        (*self) as u32
    }
}

impl From<u32> for ProposalSuccessState {
    fn from(value: u32) -> Self {
        match value {
            0 => Self::Unknown,
            1 => Self::Unsuccessful,
            2 => Self::Successful,
            _ => Self::Unknown, // Default to Unknown for invalid values
        }
    }
}

/// Proposal monitoring data for a height.
#[derive(Clone, Debug)]
pub struct ProposalMonitor {
    /// The height being monitored.
    pub height: Height,
    /// The height's primary (round-0) proposer.
    pub proposer: Address,
    /// When the height started.
    pub start_time: SystemTime,
    /// When the proposal was received. None if not received.
    pub proposal_receive_time: Option<SystemTime>,
    /// The value ID of the proposal. None if not received.
    pub value_id: Option<ValueId>,
    /// Whether the proposal was decided.
    /// This field is meaningless when `synced == true`.
    pub successful: ProposalSuccessState,
    /// Whether this height's decision came from sync.
    /// If true, timing data is not meaningful.
    pub synced: bool,
}

impl ProposalMonitor {
    /// Create a new monitor for a height.
    pub fn new(height: Height, proposer: Address, start_time: SystemTime) -> Self {
        Self {
            height,
            proposer,
            start_time,
            proposal_receive_time: None,
            value_id: None,
            successful: ProposalSuccessState::Unknown,
            synced: false,
        }
    }

    /// Record that proposal was received.
    /// Takes precedence over synced value.
    pub fn record_proposal(&mut self, value_id: ValueId) {
        // FIXME: this log message should not be produced here.
        if let Some(first_value) = self.value_id
            && !self.synced
        {
            warn!(
                height = %self.height,
                %first_value,
                new_value = %value_id,
                "Equivocating proposal at round 0"
            );
            return;
        }
        self.proposal_receive_time = Some(SystemTime::now());
        self.value_id = Some(value_id);
        self.synced = false;
    }

    /// Check if the decided value matches the recorded proposal.
    fn is_successful(&self, decided_value_id: &ValueId) -> bool {
        self.value_id
            .as_ref()
            .map(|v| v == decided_value_id)
            .unwrap_or(false)
    }

    /// Mark the proposal as successful or not depending on the decided value.
    pub fn mark_decided(&mut self, decided_value_id: &ValueId) {
        self.successful = if self.is_successful(decided_value_id) {
            ProposalSuccessState::Successful
        } else {
            ProposalSuccessState::Unsuccessful
        };
    }

    /// Mark the height has decided via sync protocol.
    pub fn mark_synced(&mut self) {
        self.synced = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::B256;
    use std::time::Duration;

    fn test_address() -> Address {
        Address::new([0x12; 20])
    }

    fn test_value_id(seed: u8) -> ValueId {
        ValueId::new(B256::repeat_byte(seed))
    }

    #[test]
    fn test_new_creates_initial_state() {
        let height = Height::new(10);
        let proposer = test_address();
        let start_time = SystemTime::now();

        let monitor = ProposalMonitor::new(height, proposer, start_time);

        assert_eq!(monitor.height, height);
        assert_eq!(monitor.proposer, proposer);
        assert_eq!(monitor.start_time, start_time);
        assert!(monitor.proposal_receive_time.is_none());
        assert!(monitor.value_id.is_none());
        assert!(monitor.successful.is_unknown());
        assert!(!monitor.synced);
    }

    #[test]
    fn test_record_proposal() {
        let before = SystemTime::now();
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), before);
        let value_id = test_value_id(0xAB);

        monitor.record_proposal(value_id);

        assert!(monitor.proposal_receive_time.unwrap() >= before);
        assert_eq!(monitor.value_id, Some(value_id));
        assert!(!monitor.synced);
    }

    #[test]
    fn test_record_proposal_overwrites_synced() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());

        let synced_time = SystemTime::now() - Duration::from_millis(50);
        monitor.proposal_receive_time = Some(synced_time);
        monitor.mark_synced();
        assert!(monitor.synced);

        // Real proposal - should overwrite
        let value_id = test_value_id(0xCD);
        monitor.record_proposal(value_id);

        assert!(monitor.proposal_receive_time.unwrap() >= synced_time);
        assert_eq!(monitor.value_id, Some(value_id));
        assert!(!monitor.synced);
    }

    #[test]
    fn test_record_proposal_duplicate_without_sync_warns() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());

        let value_id1 = test_value_id(0x11);
        monitor.record_proposal(value_id1);
        let time1 = monitor.proposal_receive_time.unwrap();

        // Second proposal, should be ignored with warning
        let value_id2 = test_value_id(0x22);
        monitor.record_proposal(value_id2);

        assert_eq!(monitor.proposal_receive_time, Some(time1));
        assert_eq!(monitor.value_id, Some(value_id1));
    }

    #[test]
    fn test_mark_decided_successful() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());
        let value_id = test_value_id(0xAA);

        monitor.record_proposal(value_id);
        monitor.mark_decided(&value_id);

        assert_eq!(monitor.successful, ProposalSuccessState::Successful);
    }

    #[test]
    fn test_mark_decided_not_successful() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());
        let proposed_value = test_value_id(0xAA);
        let decided_value = test_value_id(0xBB);

        monitor.record_proposal(proposed_value);
        monitor.mark_decided(&decided_value);

        assert_eq!(monitor.successful, ProposalSuccessState::Unsuccessful);
    }

    #[test]
    fn test_mark_decided_no_value_id() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());
        let decided_value = test_value_id(0xCC);

        // Don't record any proposal
        monitor.mark_decided(&decided_value);

        assert_eq!(monitor.successful, ProposalSuccessState::Unsuccessful);
    }

    #[test]
    fn test_mark_synced() {
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), SystemTime::now());
        assert!(!monitor.synced);

        monitor.mark_synced();

        assert!(monitor.synced);
    }
}
