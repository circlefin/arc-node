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

use crate::versions::SchemaVersion;

use super::Migrator;
use crate::decoder::DecodeError;
use crate::store::StoreError;
use crate::versions::{
    CommitCertificateVersion, ConsensusBlockVersion, ExecutionPayloadVersion, ProposalPartsVersion,
};

/// Set of migrators for all tables at a specific schema version
pub struct MigratorSet {
    pub certificate: Box<dyn Migrator>,
    pub decided_block: Box<dyn Migrator>,
    pub undecided_block: Box<dyn Migrator>,
    pub pending_parts: Box<dyn Migrator>,
}

impl MigratorSet {
    /// Get migrators for schema version 1
    /// This handles the initial version where data already has version bytes
    pub fn v1() -> Self {
        Self {
            certificate: Box::new(CommitCertificateMigrator0To1),
            decided_block: Box::new(ExecutionPayloadMigrator0To3),
            undecided_block: Box::new(ConsensusBlockMigrator0To1),
            pending_parts: Box::new(ProposalPartsMigrator0To1),
        }
    }
}

/// No-op migrator for commit certificates (v0 -> v1)
/// This is used when the database is first created or has data without version bytes
pub struct CommitCertificateMigrator0To1;

impl Migrator for CommitCertificateMigrator0To1 {
    fn name(&self) -> &str {
        "CommitCertificate v0->v1"
    }

    fn source_version(&self) -> SchemaVersion {
        SchemaVersion::V0
    }

    fn target_version(&self) -> SchemaVersion {
        SchemaVersion::new(CommitCertificateVersion::V1 as u8)
    }

    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        // For initial migration, data is already in correct format
        // Just add version byte if missing
        if old_bytes.is_empty() {
            return Err(StoreError::Decode(DecodeError::EmptyVersion));
        }

        // Unconditionally prepend version byte. Otherwise, we might have some
        // false positives when going from unversioned to versioned
        // data.
        let mut result = Vec::with_capacity(1 + old_bytes.len());
        result.push(self.target_version().as_u8());
        result.extend_from_slice(old_bytes);
        Ok(result)
    }

    fn needs_migration(&self, _bytes: &[u8]) -> bool {
        true
    }
}

/// No-op migrator for execution payloads (v0 -> v3)
/// Note: We start at v3 for execution payloads since we use ExecutionPayloadV3
pub struct ExecutionPayloadMigrator0To3;

impl Migrator for ExecutionPayloadMigrator0To3 {
    fn name(&self) -> &str {
        "ExecutionPayload v0->v3"
    }

    fn source_version(&self) -> SchemaVersion {
        SchemaVersion::V0
    }

    fn target_version(&self) -> SchemaVersion {
        SchemaVersion::new(ExecutionPayloadVersion::V3 as u8)
    }

    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        if old_bytes.is_empty() {
            return Err(StoreError::Decode(DecodeError::EmptyVersion));
        }

        // Unconditionally prepend version byte. Otherwise, we might have some
        // false positives when going from unversioned to versioned
        // data.
        let mut result = Vec::with_capacity(1 + old_bytes.len());
        result.push(self.target_version().as_u8());
        result.extend_from_slice(old_bytes);
        Ok(result)
    }

    fn needs_migration(&self, _bytes: &[u8]) -> bool {
        true
    }
}

/// No-op migrator for consensus blocks (v0 -> v1)
pub struct ConsensusBlockMigrator0To1;

impl Migrator for ConsensusBlockMigrator0To1 {
    fn name(&self) -> &str {
        "ConsensusBlock v0->v1"
    }

    fn source_version(&self) -> SchemaVersion {
        SchemaVersion::V0
    }

    fn target_version(&self) -> SchemaVersion {
        SchemaVersion::new(ConsensusBlockVersion::V1 as u8)
    }

    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        if old_bytes.is_empty() {
            return Err(StoreError::Decode(DecodeError::EmptyVersion));
        }

        // Unconditionally prepend version byte. Otherwise, we might have some
        // false positives when going from unversioned to versioned
        // data.
        let mut result = Vec::with_capacity(1 + old_bytes.len());
        result.push(self.target_version().as_u8());
        result.extend_from_slice(old_bytes);
        Ok(result)
    }

    fn needs_migration(&self, _bytes: &[u8]) -> bool {
        true
    }
}

/// No-op migrator for proposal parts (v0 -> v1)
pub struct ProposalPartsMigrator0To1;

impl Migrator for ProposalPartsMigrator0To1 {
    fn name(&self) -> &str {
        "ProposalParts v0->v1"
    }

    fn source_version(&self) -> SchemaVersion {
        SchemaVersion::V0
    }

    fn target_version(&self) -> SchemaVersion {
        SchemaVersion::new(ProposalPartsVersion::V1 as u8)
    }

    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        if old_bytes.is_empty() {
            return Err(StoreError::Decode(DecodeError::EmptyVersion));
        }

        // Unconditionally prepend version byte. Otherwise, we might have some
        // false positives when going from unversioned to versioned
        // data.
        let mut result = Vec::with_capacity(1 + old_bytes.len());
        result.push(self.target_version().as_u8());
        result.extend_from_slice(old_bytes);
        Ok(result)
    }

    fn needs_migration(&self, _bytes: &[u8]) -> bool {
        true
    }
}

// Example of a real migration (v1 -> v2) for future use:
/*
pub struct CommitCertificateMigrator1To2;

impl Migrator for CommitCertificateMigrator1To2 {
    fn name(&self) -> &str {
        "CommitCertificate v1->v2"
    }

    fn source_version(&self) -> SchemaVersion {
        SchemaVersion::new(CommitCertificateVersion::V1 as u8)
    }

    fn target_version(&self) -> SchemaVersion {
        SchemaVersion::new(CommitCertificateVersion::V2 as u8)
    }

    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        use crate::decoder::decode_certificate_v1;
        use crate::encoder::encode_certificate_v2;

        // 1. Decode using v1 decoder
        let cert = decode_certificate_v1(old_bytes)?;

        // 2. Transform data structure (e.g., add new fields, change format)
        let updated_cert = transform_certificate_v1_to_v2(cert);

        // 3. Encode using v2 encoder
        let new_bytes = encode_certificate_v2(&updated_cert)?;

        Ok(new_bytes)
    }
}
*/

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migrator_set_v1() {
        let migrators = MigratorSet::v1();

        // Verify all migrators are present
        assert_eq!(migrators.certificate.name(), "CommitCertificate v0->v1");
        assert_eq!(migrators.decided_block.name(), "ExecutionPayload v0->v3");
        assert_eq!(migrators.undecided_block.name(), "ConsensusBlock v0->v1");
        assert_eq!(migrators.pending_parts.name(), "ProposalParts v0->v1");
    }

    #[test]
    fn test_commit_certificate_migrator_versions() {
        let migrator = CommitCertificateMigrator0To1;

        assert_eq!(migrator.source_version(), SchemaVersion::V0);
        assert_eq!(
            migrator.target_version(),
            SchemaVersion::new(CommitCertificateVersion::V1 as u8)
        );
    }

    #[test]
    fn test_commit_certificate_migrator_migrate() {
        let migrator = CommitCertificateMigrator0To1;
        let target = CommitCertificateVersion::V1 as u8;

        // Empty bytes -> error
        let result = migrator.migrate(&[]);
        assert!(result.is_err());

        let data = vec![5, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result[0], target);
        assert_eq!(result.len(), 1 + data.len());
    }

    #[test]
    fn test_execution_payload_migrator_versions() {
        let migrator = ExecutionPayloadMigrator0To3;

        assert_eq!(migrator.source_version(), SchemaVersion::V0);
        assert_eq!(
            migrator.target_version(),
            SchemaVersion::new(ExecutionPayloadVersion::V3 as u8)
        );
    }

    #[test]
    fn test_execution_payload_migrator_migrate() {
        let migrator = ExecutionPayloadMigrator0To3;
        let target = ExecutionPayloadVersion::V3 as u8;

        // Empty bytes -> error
        let result = migrator.migrate(&[]);
        assert!(result.is_err());

        let data = vec![5, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result[0], target);
        assert_eq!(result.len(), 1 + data.len());
    }

    #[test]
    fn test_consensus_block_migrator_versions() {
        let migrator = ConsensusBlockMigrator0To1;

        assert_eq!(migrator.source_version(), SchemaVersion::V0);
        assert_eq!(
            migrator.target_version(),
            SchemaVersion::new(ConsensusBlockVersion::V1 as u8)
        );
    }

    #[test]
    fn test_consensus_block_migrator_migrate() {
        let migrator = ConsensusBlockMigrator0To1;
        let target = ConsensusBlockVersion::V1 as u8;

        // Empty bytes -> error
        let result = migrator.migrate(&[]);
        assert!(result.is_err());

        let data = vec![5, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result[0], target);
        assert_eq!(result.len(), 1 + data.len());
    }

    #[test]
    fn test_proposal_parts_migrator_versions() {
        let migrator = ProposalPartsMigrator0To1;

        assert_eq!(migrator.source_version(), SchemaVersion::V0);
        assert_eq!(
            migrator.target_version(),
            SchemaVersion::new(ProposalPartsVersion::V1 as u8)
        );
    }

    #[test]
    fn test_proposal_parts_migrator_migrate() {
        let migrator = ProposalPartsMigrator0To1;
        let target = ProposalPartsVersion::V1 as u8;

        // Empty bytes -> error
        let result = migrator.migrate(&[]);
        assert!(result.is_err());

        let data = vec![5, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result[0], target);
        assert_eq!(result.len(), 1 + data.len());
    }

    #[test]
    fn test_all_migrators_consistent() {
        // All migrators should have source version 0 for initial migration
        let migrators = MigratorSet::v1();

        assert_eq!(migrators.certificate.source_version(), SchemaVersion::V0);
        assert_eq!(migrators.decided_block.source_version(), SchemaVersion::V0);
        assert_eq!(
            migrators.undecided_block.source_version(),
            SchemaVersion::V0
        );
        assert_eq!(migrators.pending_parts.source_version(), SchemaVersion::V0);

        // All should have proper target versions
        assert_eq!(
            migrators.certificate.target_version(),
            SchemaVersion::new(CommitCertificateVersion::V1 as u8)
        );
        assert_eq!(
            migrators.decided_block.target_version(),
            SchemaVersion::new(ExecutionPayloadVersion::V3 as u8)
        );
        assert_eq!(
            migrators.undecided_block.target_version(),
            SchemaVersion::new(ConsensusBlockVersion::V1 as u8)
        );
        assert_eq!(
            migrators.pending_parts.target_version(),
            SchemaVersion::new(ProposalPartsVersion::V1 as u8)
        );
    }

    #[test]
    fn test_migrator_replaces_version_byte() {
        // Test that migration replaces the version byte but preserves rest of data
        let old_version = 0;
        let test_data = vec![old_version, 1, 2, 3, 4, 5, 6, 7, 8, 9];

        let certificate_migrator = CommitCertificateMigrator0To1;
        let result = certificate_migrator.migrate(&test_data).unwrap();

        // First byte should be the new version
        assert_eq!(result[0], CommitCertificateVersion::V1 as u8);

        // Rest should be the same
        assert_eq!(result.len(), 1 + test_data.len());
        assert_eq!(&result[1..], &test_data[..]);
    }
}
