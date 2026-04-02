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

/// Signed consensus message version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedConsensusMsgVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for SignedConsensusMsgVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Liveness message version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessMsgVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for LivenessMsgVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Proposal part version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalPartVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ProposalPartVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Stream message version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMessageVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for StreamMessageVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Sync status version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatusVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for SyncStatusVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Sync request version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRequestVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for SyncRequestVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Sync response version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncResponseVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for SyncResponseVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Proposed value version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposedValueVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ProposedValueVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Validator proof version
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorProofVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ValidatorProofVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signed_consensus_msg_version_try_from_v1() {
        let result = SignedConsensusMsgVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), SignedConsensusMsgVersion::V1);
    }

    #[test]
    fn test_signed_consensus_msg_version_try_from_unknown() {
        let result = SignedConsensusMsgVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_liveness_msg_version_try_from_v1() {
        let result = LivenessMsgVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), LivenessMsgVersion::V1);
    }

    #[test]
    fn test_liveness_msg_version_try_from_unknown() {
        let result = LivenessMsgVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_proposal_part_version_try_from_v1() {
        let result = ProposalPartVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ProposalPartVersion::V1);
    }

    #[test]
    fn test_proposal_part_version_try_from_unknown() {
        let result = ProposalPartVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_stream_message_version_try_from_v1() {
        let result = StreamMessageVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), StreamMessageVersion::V1);
    }

    #[test]
    fn test_stream_message_version_try_from_unknown() {
        let result = StreamMessageVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_sync_status_version_try_from_v1() {
        let result = SyncStatusVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), SyncStatusVersion::V1);
    }

    #[test]
    fn test_sync_status_version_try_from_unknown() {
        let result = SyncStatusVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_sync_request_version_try_from_v1() {
        let result = SyncRequestVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), SyncRequestVersion::V1);
    }

    #[test]
    fn test_sync_request_version_try_from_unknown() {
        let result = SyncRequestVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_sync_response_version_try_from_v1() {
        let result = SyncResponseVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), SyncResponseVersion::V1);
    }

    #[test]
    fn test_sync_response_version_try_from_unknown() {
        let result = SyncResponseVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_proposed_value_version_try_from_v1() {
        let result = ProposedValueVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ProposedValueVersion::V1);
    }

    #[test]
    fn test_proposed_value_version_try_from_unknown() {
        let result = ProposedValueVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }

    #[test]
    fn test_validator_proof_version_try_from_v1() {
        let result = ValidatorProofVersion::try_from(0x01);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ValidatorProofVersion::V1);
    }

    #[test]
    fn test_validator_proof_version_try_from_unknown() {
        let result = ValidatorProofVersion::try_from(0x02);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 0x02);
    }
}
