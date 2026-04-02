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

use std::collections::BTreeMap;
use std::mem::size_of;
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::invalid_payloads::InvalidPayload;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use bytesize::ByteSize;
use redb::{ReadableTable, ReadableTableMetadata, WriteTransaction};
use thiserror::Error;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use arc_consensus_types::evidence::StoredMisbehaviorEvidence;
use arc_consensus_types::{
    Address, ArcContext, BlockHash, CommitCertificateType, Height, ProposalParts,
    StoredCommitCertificate, B256,
};
use malachitebft_app_channel::app::types::core::{CommitCertificate, Round};
use malachitebft_core_types::Height as _;
use malachitebft_proto::Error as ProtoError;

use crate::decoder::{
    decode_block, decode_certificate, decode_execution_payload, decode_invalid_payloads,
    decode_misbehavior_evidence, decode_proposal_monitor_data, decode_proposal_parts, DecodeError,
};
use crate::encoder::{
    encode_block, encode_certificate, encode_execution_payload, encode_invalid_payloads,
    encode_misbehavior_evidence, encode_proposal_monitor_data, encode_proposal_parts,
};
use crate::invalid_payloads::StoredInvalidPayloads;
use crate::keys::{HeightKey, PendingPartsKey, UndecidedBlockKey};
use crate::metrics::DbMetrics;
use arc_consensus_types::block::{ConsensusBlock, DecidedBlock};
use arc_consensus_types::proposal_monitor::ProposalMonitor;

/// Store error.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Database error: {0}")]
    Database(#[from] redb::DatabaseError),

    #[error("Database upgrade error: {0}")]
    Upgrade(#[from] redb::UpgradeError),

    #[error("Storage error: {0}")]
    Storage(#[from] redb::StorageError),

    #[error("Table error: {0}")]
    Table(#[from] redb::TableError),

    #[error("Commit error: {0}")]
    Commit(#[from] redb::CommitError),

    #[error("Transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),

    #[error("Failed to encode/decode Protobuf: {0}")]
    Protobuf(#[from] ProtoError),

    #[error("Failed to decode: {0}")]
    Decode(#[from] DecodeError),

    #[error("Failed to join on task: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl StoreError {
    pub fn other<E>(error: E) -> Self
    where
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        StoreError::Other(error.into())
    }
}

pub const CERTIFICATES_TABLE: redb::TableDefinition<HeightKey, Vec<u8>> =
    redb::TableDefinition::new("certificates");

pub const DECIDED_BLOCKS_TABLE: redb::TableDefinition<HeightKey, Vec<u8>> =
    redb::TableDefinition::new("decided_blocks");

pub const UNDECIDED_BLOCKS_TABLE: redb::TableDefinition<UndecidedBlockKey, Vec<u8>> =
    redb::TableDefinition::new("undecided_blocks");

pub const PENDING_PROPOSAL_PARTS_TABLE: redb::TableDefinition<PendingPartsKey, Vec<u8>> =
    redb::TableDefinition::new("pending_proposal_parts");

pub const MISBEHAVIOR_EVIDENCE_TABLE: redb::TableDefinition<HeightKey, Vec<u8>> =
    redb::TableDefinition::new("misbehavior_evidence");

pub const INVALID_PAYLOADS_TABLE: redb::TableDefinition<HeightKey, Vec<u8>> =
    redb::TableDefinition::new("invalid_payloads");

pub const PROPOSAL_MONITOR_DATA_TABLE: redb::TableDefinition<HeightKey, Vec<u8>> =
    redb::TableDefinition::new("proposal_monitor_data");

#[tracing::instrument(name = "db.monitor", skip_all)]
async fn monitor_db_size(path: PathBuf, metrics: DbMetrics, interval: Duration) {
    let mut interval = tokio::time::interval(interval);

    loop {
        // NOTE: The first tick completes immediately
        interval.tick().await;

        match tokio::fs::metadata(&path).await {
            Ok(metadata) => {
                let size = metadata.len();
                metrics.set_db_size(size);
            }
            Err(e) => {
                error!("Failed to get database size: {e}");
            }
        }
    }
}

struct Db {
    db: redb::Database,
    path: PathBuf,
    metrics: DbMetrics,
}

impl Db {
    /// Number of blocks `reth` is expected to forget upon recovery in the worst case.
    const RETH_AMNESIA_HEIGHT_COUNT: u64 = 10_000;
    /// Maximum number of records to prune per invocation.
    const PRUNE_BATCH_LIMIT: usize = 300;
    /// How many heights before pruning logs info messages.
    const PRUNING_LOG_INFO_HEIGHTS: u64 = 100;

    #[tracing::instrument(name = "db", skip_all)]
    fn new(
        path: impl AsRef<Path>,
        metrics: DbMetrics,
        db_upgrade: DbUpgrade,
        cache_size: ByteSize,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref().to_owned();
        let db_exists = path.exists();

        let mut db = redb::Database::builder()
            .set_cache_size(cache_size.as_u64() as usize)
            .set_repair_callback(|session| {
                let status = session.progress() * 100.0;
                info!("Database repair in progress: {status:.2}%");
            })
            .create(&path)?;

        info!(path = %path.display(), "Database opened");

        // Upgrade the database file format to v3 for forward compatibility with redb 3.x.
        // This is a one-time operation that makes the database file readable by redb 3.x.
        if db.upgrade()? {
            info!("Upgraded database to v3 file format");
        } else {
            debug!("Database already in v3 file format");
        }

        // Perform schema migration if needed
        let coordinator = crate::migrations::MigrationCoordinator::new(db);

        if db_upgrade == DbUpgrade::Skip {
            info!("Skipping database schema upgrade as requested");
        } else if coordinator.needs_migration(db_exists)? {
            info!("Database schema migration required");
            let stats = coordinator.migrate()?;

            info!(
                tables = stats.tables_migrated,
                scanned = stats.records_scanned,
                upgraded = stats.records_upgraded,
                skipped = stats.records_skipped,
                duration = ?stats.duration,
                "Database upgrade completed successfully"
            );
        }

        // Retrieve the database from the coordinator after migration
        let db = coordinator.into_db();

        Ok(Self { db, path, metrics })
    }

    /// Spawn a background task to monitor the database size at regular intervals.
    fn spawn_monitor(&self, interval: Duration) -> JoinHandle<()> {
        tokio::task::spawn(monitor_db_size(
            self.path.clone(),
            self.metrics.clone(),
            interval,
        ))
    }

    fn get_payload(&self, height: Height) -> Result<Option<ExecutionPayloadV3>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;

        let table = tx.open_table(DECIDED_BLOCKS_TABLE)?;
        let payload = table.get(&height)?;
        let payload = payload
            .map(|value| {
                let bytes = value.value();
                read_bytes += bytes.len();
                decode_execution_payload(&bytes)
            })
            .transpose()
            .map_err(StoreError::from)?;

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        Ok(payload)
    }

    /// Get the commit certificate for the given height.
    /// - height: The height to get the certificate for. If None, get the latest certificate.
    fn get_certificate(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredCommitCertificate>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(CERTIFICATES_TABLE)?;

        let bytes = if let Some(height) = height {
            table.get(&height)?.map(|v| v.value())
        } else {
            table.last()?.map(|(_, v)| v.value())
        };

        let result = bytes.and_then(|bytes| {
            read_bytes += bytes.len();
            decode_certificate(&bytes).ok()
        });

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        Ok(result)
    }

    /// Get the decided block for the given height.
    fn get_decided_block(&self, height: Height) -> Result<Option<DecidedBlock>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;

        let payload = {
            let table = tx.open_table(DECIDED_BLOCKS_TABLE)?;
            let payload = table.get(&height)?.map(|value| value.value());
            payload
                .map(|bytes| {
                    read_bytes += bytes.len();
                    decode_execution_payload(&bytes)
                })
                .transpose()
                .map_err(StoreError::from)?
        };

        let certificate = {
            let table = tx.open_table(CERTIFICATES_TABLE)?;
            let value = table.get(&height)?;
            value.and_then(|value| {
                let bytes = value.value();
                read_bytes += bytes.len();
                decode_certificate(&bytes).ok()
            })
        };

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        let decided_block = payload.zip(certificate).map(|(execution_payload, cert)| {
            DecidedBlock::new(execution_payload, cert.certificate)
        });

        Ok(decided_block)
    }

    /// Store the decided block and its certificate.
    fn insert_decided_block(
        &self,
        decided_block: DecidedBlock,
        proposer: Address,
    ) -> Result<(), StoreError> {
        let start = Instant::now();
        let mut write_bytes = 0;

        let height = decided_block.height();
        let tx = self.db.begin_write()?;

        {
            let mut blocks = tx.open_table(DECIDED_BLOCKS_TABLE)?;
            let block_bytes = encode_execution_payload(&decided_block.execution_payload);
            write_bytes += block_bytes.len();
            blocks.insert(height, block_bytes)?;
        }

        self.insert_certificate(
            &tx,
            decided_block.certificate,
            CommitCertificateType::Minimal,
            Some(proposer),
        )?;

        tx.commit()?;

        self.update_write_metrics(write_bytes, start.elapsed());

        Ok(())
    }

    /// Extend an existing commit certificate with additional precommit signatures collected during finalization.
    fn extend_certificate(
        &self,
        certificate: CommitCertificate<ArcContext>,
    ) -> Result<(), StoreError> {
        let height = certificate.height;
        let tx = self.db.begin_write()?;

        // Check that a certificate already exists for this height and only allow insert if it does.
        let existing = {
            let table = tx.open_table(CERTIFICATES_TABLE)?;
            let bytes = table.get(&height)?.map(|v| v.value()).ok_or_else(||
                StoreError::other(format!(
                    "Cannot extend certificate for height {height} because no existing certificate was found"
                )))?;

            decode_certificate(&bytes).map_err(|e| {
                StoreError::other(format!(
                    "Failed to decode existing certificate for height {height}: {e}"
                ))
            })?
        };

        // Check that the new certificate is a valid extension of the existing one:
        // same height, same round, same value, superset of commit signatures.

        if certificate.height != existing.certificate.height {
            return Err(StoreError::other(format!(
                "Cannot extend certificate for height {height} because existing certificate has different height {}",
                existing.certificate.height
            )));
        }

        if certificate.round != existing.certificate.round {
            return Err(StoreError::other(format!(
                "Cannot extend certificate for height {height} because existing certificate has different round {}",
                existing.certificate.round
            )));
        }

        if certificate.value_id != existing.certificate.value_id {
            return Err(StoreError::other(format!(
                "Cannot extend certificate for height {height} because existing certificate has different value_id {}",
                existing.certificate.value_id
            )));
        }

        for signature in &existing.certificate.commit_signatures {
            if !certificate.commit_signatures.contains(signature) {
                return Err(StoreError::other(format!(
                    "Cannot extend certificate for height {height} because existing \
                    commit signature from validator {} is missing in the new certificate",
                    signature.address
                )));
            }
        }

        self.insert_certificate(
            &tx,
            certificate,
            CommitCertificateType::Extended,
            existing.proposer,
        )?;

        tx.commit()?;

        Ok(())
    }

    fn insert_certificate(
        &self,
        tx: &WriteTransaction,
        certificate: CommitCertificate<ArcContext>,
        certificate_type: CommitCertificateType,
        proposer: Option<Address>,
    ) -> Result<(), StoreError> {
        let start = Instant::now();

        let height = certificate.height;

        let stored = StoredCommitCertificate {
            certificate,
            certificate_type,
            proposer,
        };

        let encoded_certificate = encode_certificate(&stored)?;
        let write_bytes = encoded_certificate.len();

        {
            let mut certificates = tx.open_table(CERTIFICATES_TABLE)?;
            certificates.insert(height, encoded_certificate)?;
        }
        self.update_write_metrics(write_bytes, start.elapsed());

        Ok(())
    }

    /// Store misbehavior evidence for a given height.
    fn insert_misbehavior_evidence(
        &self,
        evidence: StoredMisbehaviorEvidence,
    ) -> Result<(), StoreError> {
        let start = Instant::now();

        let height = evidence.height;
        let tx = self.db.begin_write()?;

        let encoded = encode_misbehavior_evidence(&evidence)?;
        let write_bytes = encoded.len();

        {
            let mut table = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE)?;
            table.insert(height, encoded)?;
        }

        tx.commit()?;

        self.update_write_metrics(write_bytes, start.elapsed());

        Ok(())
    }

    /// Store or update proposal monitor data for a given height.
    fn insert_proposal_monitor_data(&self, data: ProposalMonitor) -> Result<(), StoreError> {
        let start = Instant::now();

        let height = data.height;
        let tx = self.db.begin_write()?;

        let encoded = encode_proposal_monitor_data(&data)?;
        let write_bytes = encoded.len();

        {
            let mut table = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE)?;
            table.insert(height, encoded)?;
        }

        tx.commit()?;

        self.update_write_metrics(write_bytes, start.elapsed());

        Ok(())
    }

    /// Get proposal monitor data for the given height.
    /// - height: The height to get the data for. If None, get the latest.
    fn get_proposal_monitor_data(
        &self,
        height: Option<Height>,
    ) -> Result<Option<ProposalMonitor>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE)?;

        let bytes = if let Some(height) = height {
            table.get(&height)?.map(|v| v.value())
        } else {
            table.last()?.map(|(_, v)| v.value())
        };

        let data = bytes
            .map(|bytes| {
                read_bytes += bytes.len();
                decode_proposal_monitor_data(&bytes)
            })
            .transpose()
            .map_err(StoreError::from)?;

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        Ok(data)
    }

    /// Get misbehavior evidence for the given height.
    /// - height: The height when the evidence was collected. If None, use the latest height.
    fn get_misbehavior_evidence(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredMisbehaviorEvidence>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let Some(max_height) = self.max_height()? else {
            // No blocks yet
            return Ok(None);
        };

        // Determine the effective height to query
        let height = match height {
            Some(height) if height > max_height => return Ok(None),
            Some(height) => height,
            None => max_height,
        };

        let tx = self.db.begin_read()?;
        let table = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE)?;

        let bytes = table.get(&height)?.map(|v| v.value());

        let evidence = bytes
            .map(|bytes| {
                read_bytes += bytes.len();
                decode_misbehavior_evidence(&bytes)
            })
            .transpose()
            .map_err(StoreError::from)?;

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        // Return empty evidence if no record found
        Ok(Some(evidence.unwrap_or_else(|| {
            StoredMisbehaviorEvidence::empty(height)
        })))
    }

    /// Get invalid payloads for the given height.
    ///
    /// - height: The height to get the invalid payloads for. If None, use the latest height.
    fn get_invalid_payloads(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredInvalidPayloads>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let Some(max_height) = self.max_height()? else {
            // No blocks yet
            return Ok(None);
        };

        // Determine the effective height to query
        let height = match height {
            Some(height) if height > max_height => return Ok(None),
            Some(height) => height,
            None => max_height,
        };

        let tx = self.db.begin_read()?;
        let table = tx.open_table(INVALID_PAYLOADS_TABLE)?;

        let bytes = table.get(&height)?.map(|v| v.value());

        let payloads = bytes
            .map(|bytes| {
                read_bytes += bytes.len();
                decode_invalid_payloads(&bytes)
            })
            .transpose()
            .map_err(StoreError::from)?;

        self.update_read_metrics(read_bytes, size_of::<Height>(), start.elapsed());

        // Return empty payloads if no record found
        Ok(Some(
            payloads.unwrap_or_else(|| StoredInvalidPayloads::empty(height)),
        ))
    }

    /// Atomically appends an invalid payload to the stored collection for its
    /// height, creating the collection if none exists yet.
    ///
    /// The read and write happen inside a single redb write transaction, so
    /// concurrent appends for the same height cannot lose data.
    fn append_invalid_payload(&self, invalid_payload: InvalidPayload) -> Result<(), StoreError> {
        let start = Instant::now();
        let height = invalid_payload.height;
        let tx = self.db.begin_write()?;

        let mut stored = {
            let table = tx.open_table(INVALID_PAYLOADS_TABLE)?;
            match table.get(&height)? {
                Some(v) => decode_invalid_payloads(&v.value())?,
                None => StoredInvalidPayloads {
                    height,
                    payloads: vec![],
                },
            }
        };

        stored.add_invalid_payload(invalid_payload);
        let encoded = encode_invalid_payloads(&stored)?;
        let write_bytes = encoded.len();

        {
            let mut table = tx.open_table(INVALID_PAYLOADS_TABLE)?;
            table.insert(height, encoded)?;
        }

        tx.commit()?;
        self.update_write_metrics(write_bytes, start.elapsed());
        Ok(())
    }

    /// Get the undecided block for the given height, round, and block hash.
    #[tracing::instrument(skip(self))]
    pub fn get_undecided_block(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;

        let value = if let Ok(Some(value)) = table.get(&(height, round, block_hash)) {
            let bytes = value.value();
            read_bytes += bytes.len();

            let block = decode_block(&bytes)?;
            Some(block)
        } else {
            None
        };

        self.update_read_metrics(
            read_bytes,
            size_of::<(Height, Round, BlockHash)>(),
            start.elapsed(),
        );

        Ok(value)
    }

    #[tracing::instrument(skip(self))]
    pub fn get_undecided_block_by_height_and_block_hash(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;

        // Iterate through all entries to find one that matches height and block_hash
        for result in table.iter()? {
            let (key, value) = result?;
            let (key_height, _key_round, key_block_hash) = key.value();

            if key_height == height && key_block_hash == block_hash {
                let bytes = value.value();
                read_bytes += bytes.len();

                let block = decode_block(&bytes)?;

                self.update_read_metrics(
                    read_bytes,
                    size_of::<(Height, BlockHash)>(),
                    start.elapsed(),
                );

                return Ok(Some(block));
            }
        }

        self.update_read_metrics(
            read_bytes,
            size_of::<(Height, BlockHash)>(),
            start.elapsed(),
        );

        Ok(None)
    }

    /// Get all undecided blocks for a given height and round (sync version)
    fn get_undecided_blocks(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ConsensusBlock>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;

        let mut blocks = Vec::new();

        // Iterate through all entries that start with (height, round, *)
        let range_start = (height, round, BlockHash::new([0; 32]));
        let range_end = (
            height,
            Round::from(round.as_i64() + 1),
            BlockHash::new([0; 32]),
        );

        for result in table.range(range_start..range_end)? {
            let (key, value) = result?;
            let key_tuple = key.value();

            // Only include entries that match exactly height and round
            if key_tuple.0 == height && key_tuple.1 == round {
                let bytes = value.value();
                read_bytes += bytes.len();

                let proposal = decode_block(&bytes)?;
                blocks.push(proposal);
            }
        }

        self.update_read_metrics(
            read_bytes,
            size_of::<(Height, Round, BlockHash)>() * blocks.len(),
            start.elapsed(),
        );

        Ok(blocks)
    }

    fn insert_undecided_block(
        &self,
        tx: &mut WriteTransaction,
        block: ConsensusBlock,
    ) -> Result<(), StoreError> {
        let start = Instant::now();

        let key = (block.height, block.round, block.block_hash());
        let value = encode_block(&block);

        {
            let mut table = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;
            table.insert(key, value.to_vec())?;
        }

        self.update_write_metrics(value.len(), start.elapsed());

        Ok(())
    }

    fn remove_pending_proposal_parts(
        &self,
        tx: &mut WriteTransaction,
        parts: ProposalParts,
    ) -> Result<(), StoreError> {
        let start = Instant::now();

        let key = (parts.height(), parts.round(), B256::new(parts.hash()));

        {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;
            table.remove(key)?;
        }

        self.update_delete_metrics(start.elapsed());

        Ok(())
    }

    fn height_range<Table>(
        &self,
        table: &Table,
        range: impl RangeBounds<Height>,
        limit: usize,
    ) -> Result<Vec<Height>, StoreError>
    where
        Table: redb::ReadableTable<HeightKey, Vec<u8>>,
    {
        Ok(table
            .range(range)?
            .take(limit)
            .flatten()
            .map(|(key, _)| key.value())
            .collect::<Vec<_>>())
    }

    fn get_pending_proposal_parts(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ProposalParts>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

        let mut proposals = Vec::new();
        for result in table.iter()? {
            let (key, value) = result?;
            let (h, r, _) = key.value();

            if h == height && r == round {
                let bytes = value.value();
                read_bytes += bytes.len();

                let parts = decode_proposal_parts(&bytes)?;
                proposals.push(parts);
            }
        }

        self.update_read_metrics(
            read_bytes,
            size_of::<(Height, Round, BlockHash)>() * proposals.len(),
            start.elapsed(),
        );

        Ok(proposals)
    }

    /// Return the total number of stored pending proposal parts.
    fn get_pending_proposal_parts_count(&self) -> Result<usize, StoreError> {
        let start = Instant::now();
        let tx = self.db.begin_read()?;
        let table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

        let count = table.len()? as usize;

        self.update_read_metrics(0, 0, start.elapsed());

        Ok(count)
    }

    /// Return the number of stored pending proposal parts grouped by height.
    /// Each entry in the returned vector contains the height and the count of
    /// proposal parts currently stored in `PENDING_PROPOSAL_PARTS_TABLE` for that
    /// height.
    fn get_pending_proposal_parts_counts(&self) -> Result<Vec<(Height, usize)>, StoreError> {
        let start = Instant::now();
        let mut read_bytes = 0usize;
        let mut counts = BTreeMap::new();
        let mut total_keys = 0usize;

        let tx = self.db.begin_read()?;
        let table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

        for result in table.iter()? {
            let (key, value) = result?;
            let (height, _, _) = key.value();

            let bytes = value.value();
            read_bytes += bytes.len();

            let count = counts.entry(height).or_insert(0);
            *count += 1;

            total_keys += 1;
        }

        self.update_read_metrics(
            read_bytes,
            size_of::<(Height, Round, BlockHash)>() * total_keys,
            start.elapsed(),
        );

        Ok(counts.into_iter().collect())
    }

    fn insert_pending_proposal_parts(
        &self,
        parts: ProposalParts,
        max_pending_parts: usize,
        current_height: Height,
    ) -> Result<bool, StoreError> {
        let start = Instant::now();

        // Calculate max allowed height (inclusive).
        // For current_height=10 and max_pending_parts=4, we allow heights 10, 11, 12, 13.
        let max_allowed_height = current_height.increment_by(max_pending_parts as u64 - 1);

        // Do not insert if proposal is outside the allowed range (too far in the future)
        if parts.height() > max_allowed_height {
            return Ok(false);
        }

        let mut inserted = false;

        let key = (parts.height(), parts.round(), B256::new(parts.hash()));
        let value = encode_proposal_parts(&parts)?;

        // Insert the proposal if there is room in the table
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

            let count = table.len()? as usize;

            if count < max_pending_parts {
                table.insert(key, value.to_vec())?;
                inserted = true;
            }
        }
        tx.commit()?;

        if inserted {
            self.update_write_metrics(value.len(), start.elapsed());
        }

        Ok(inserted)
    }

    /// Enforce pending proposals limit on startup.
    /// Called during Store::open to clean up any excess proposals from previous runs.
    /// Removes all proposals outside the valid range and trims to max_pending_proposals.
    fn enforce_pending_proposals_limit(
        &self,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<Vec<(Height, Round, BlockHash)>, StoreError> {
        let tx = self.db.begin_write()?;

        let removed = {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

            // Calculate max allowed height (inclusive).
            // For current_height=10 and max_pending_parts=4, we allow heights 10, 11, 12, 13.
            let max_allowed_height = current_height.increment_by(max_pending_proposals as u64 - 1);

            // Collect all keys, categorize as: stale, within_range, or too_far
            // Keys are sorted by (height, round, hash) ascending
            let (stale, within_range, too_far) = table
                .iter()?
                .filter_map(|result| result.ok().map(|(k, _)| k.value()))
                .fold(
                    (vec![], vec![], vec![]),
                    |(mut stale, mut within, mut far), key| {
                        let (height, _, _) = key;
                        if height < current_height {
                            stale.push(key);
                        } else if height <= max_allowed_height {
                            within.push(key);
                        } else {
                            far.push(key);
                        }
                        (stale, within, far)
                    },
                );

            // Determine keys to remove:
            // - all stale entries from previous heights
            // - all entries that are too far in the future
            // - entries beyond max_pending_proposals within valid range (keep lowest, remove highest)
            let mut keys_to_remove = stale;
            keys_to_remove.extend(too_far);

            if within_range.len() > max_pending_proposals {
                // within_range is sorted by (height, round, hash) ascending
                // Keep first max_pending_proposals, remove the rest (highest heights/rounds)
                keys_to_remove.extend(within_range.iter().skip(max_pending_proposals).copied());
            }

            if !keys_to_remove.is_empty() {
                info!(
                    entries_to_remove = keys_to_remove.len(),
                    max_pending_proposals,
                    current_height = %current_height,
                    %max_allowed_height,
                    "Cleaning proposals on startup"
                );

                for key_to_remove in &keys_to_remove {
                    table.remove(key_to_remove)?;
                }
            }

            keys_to_remove
        };

        tx.commit()?;

        Ok(removed)
    }

    /// Clean up undecided blocks and pending proposals for heights <= current_height.
    /// This should always run when committing a block, regardless of pruning configuration.
    fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), StoreError> {
        let start = Instant::now();

        let tx = self.db.begin_write()?;

        {
            // Remove all undecided blocks with height <= current_height
            let mut undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;
            undecided.retain(|k, _| k.0 > current_height)?;

            // Remove all pending proposals with height <= current_height
            let mut pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;
            pending.retain(|k, _| k.0 > current_height)?;
        }

        tx.commit()?;

        self.metrics.observe_delete_time(start.elapsed());

        Ok(())
    }

    /// Prune up to PRUNE_BATCH_LIMIT historical certificates below retain_height.
    /// This should only run when pruning is enabled.
    fn prune_historical_certs(&self, retain_height: Height) -> Result<Vec<Height>, StoreError> {
        let start = Instant::now();

        let curr_height = self.max_height()?.unwrap_or_default();
        let log_info = curr_height.as_u64() % Self::PRUNING_LOG_INFO_HEIGHTS == 0;

        let keys = {
            let tx_read = self.db.begin_read()?;
            let certificates = tx_read.open_table(CERTIFICATES_TABLE)?;
            self.height_range(&certificates, ..retain_height, Self::PRUNE_BATCH_LIMIT)?
        };

        if keys.is_empty() {
            if log_info {
                info!(%retain_height, %curr_height, "No historical certificates to prune in this batch");
            } else {
                debug!(%retain_height, %curr_height, "No historical certificates to prune in this batch");
            }
            self.update_delete_metrics(start.elapsed());
            return Ok(keys);
        }

        // Remove collected keys within a short write transaction
        let tx_write = self.db.begin_write()?;
        {
            let mut certificates = tx_write.open_table(CERTIFICATES_TABLE)?;
            for h in &keys {
                let _ = certificates.remove(h)?;
            }
        }
        tx_write.commit()?;

        let first_pruned = keys.first().expect("'keys' should not be empty").as_u64();
        let last_pruned = keys.last().expect("'keys' should not be empty").as_u64();
        if log_info {
            info!(
                pruned_count = keys.len(),
                %retain_height,
                current_height = %curr_height,
                %first_pruned,
                %last_pruned,
                "Pruned historical certificates batch"
            );
        } else {
            debug!(
                pruned_count = keys.len(),
                %retain_height,
                current_height = %curr_height,
                %first_pruned,
                %last_pruned,
                "Pruned historical certificates batch"
            );
        }

        self.update_delete_metrics(start.elapsed());

        Ok(keys)
    }

    /// Prune up to PRUNE_BATCH_LIMIT blocks below (current_height - `EL_AMNESIA_HEIGHT_COUNT`).
    /// This should run regardless of whether pruning is enabled.
    fn prune_blocks(&self) -> Result<Vec<Height>, StoreError> {
        let start = Instant::now();

        let curr_height = self.max_height()?.unwrap_or_default();
        let log_info = curr_height.as_u64() % Self::PRUNING_LOG_INFO_HEIGHTS == 0;
        let retain_height = curr_height.saturating_sub(Self::RETH_AMNESIA_HEIGHT_COUNT);

        let keys = {
            let tx_read = self.db.begin_read()?;
            let decided = tx_read.open_table(DECIDED_BLOCKS_TABLE)?;
            self.height_range(&decided, ..retain_height, Self::PRUNE_BATCH_LIMIT)?
        };

        if keys.is_empty() {
            if log_info {
                info!(%retain_height, %curr_height, "No decided blocks to prune in this batch");
            } else {
                debug!(%retain_height, %curr_height, "No decided blocks to prune in this batch");
            }
            self.update_delete_metrics(start.elapsed());
            return Ok(keys);
        }

        // Remove collected keys within a short write transaction
        let tx_write = self.db.begin_write()?;
        {
            let mut decided = tx_write.open_table(DECIDED_BLOCKS_TABLE)?;
            for h in &keys {
                let _ = decided.remove(h)?;
            }
        }
        tx_write.commit()?;

        let first_pruned = keys.first().expect("'keys' should not be empty").as_u64();
        let last_pruned = keys.last().expect("'keys' should not be empty").as_u64();

        if log_info {
            info!(
                pruned_count = keys.len(),
                %retain_height,
                current_height = %curr_height,
                %first_pruned,
                %last_pruned,
                "Pruned decided blocks batch"
            );
        } else {
            debug!(
                pruned_count = keys.len(),
                %retain_height,
                current_height = %curr_height,
                %first_pruned,
                %last_pruned,
                "Pruned decided blocks batch"
            );
        }

        self.update_delete_metrics(start.elapsed());

        Ok(keys)
    }

    fn limit_height(&self, min: bool) -> Result<Option<Height>, StoreError> {
        let start = Instant::now();
        let tx = self.db.begin_read()?;
        let table = tx.open_table(CERTIFICATES_TABLE)?;

        let maybe_key = if min { table.first()? } else { table.last()? };
        if let Some((key, block)) = maybe_key {
            self.update_read_metrics(block.value().len(), size_of::<Height>(), start.elapsed());

            return Ok(Some(key.value()));
        }

        Ok(None)
    }

    fn min_height(&self) -> Result<Option<Height>, StoreError> {
        self.limit_height(true)
    }

    fn max_height(&self) -> Result<Option<Height>, StoreError> {
        self.limit_height(false)
    }

    /// Create a savepoint in the database to ensure the allocator state table is up to date.
    /// Doing this before shutting down the database can help avoid repair on next startup.
    #[tracing::instrument(name = "db::savepoint", skip_all)]
    fn savepoint(&self) {
        if self.ensure_allocator_state_table().is_err() {
            warn!("Failed to write allocator state table. Repair may be required at restart.");
        }
    }

    /// Make a new quick-repair commit to update the allocator state table
    fn ensure_allocator_state_table(&self) -> Result<(), StoreError> {
        debug!("Writing allocator state table");

        let mut tx = self.db.begin_write()?;
        tx.set_quick_repair(true);
        tx.commit()?;

        Ok(())
    }

    fn create_tables(&self) -> Result<(), StoreError> {
        let tx = self.db.begin_write()?;

        // Implicitly creates the tables if they do not exist yet
        let _ = tx.open_table(CERTIFICATES_TABLE)?;
        let _ = tx.open_table(DECIDED_BLOCKS_TABLE)?;
        let _ = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;
        let _ = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;
        let _ = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE)?;
        let _ = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE)?;
        let _ = tx.open_table(INVALID_PAYLOADS_TABLE)?;

        tx.commit()?;

        Ok(())
    }

    fn update_read_metrics(&self, read_bytes: usize, key_read_bytes: usize, read_time: Duration) {
        self.metrics.add_read_bytes(read_bytes as u64);
        self.metrics.add_key_read_bytes(key_read_bytes as u64);
        self.metrics.observe_read_time(read_time);
    }

    fn update_write_metrics(&self, write_bytes: usize, write_time: Duration) {
        self.metrics.add_write_bytes(write_bytes as u64);
        self.metrics.observe_write_time(write_time);
    }

    fn update_delete_metrics(&self, delete_time: Duration) {
        self.metrics.observe_delete_time(delete_time);
    }
}

/// Whether to perform database schema upgrade on startup or skip it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DbUpgrade {
    /// Skip database schema upgrade on startup.
    Skip,

    /// Perform database schema upgrade on startup if needed.
    #[default]
    Perform,
}

/// Internal store for the application data.
#[derive(Clone)]
pub struct Store {
    db: Arc<Db>,
}

impl Store {
    /// Open the store.
    /// - path: The path to the store.
    /// - metrics: The metrics to use for the store.
    /// - skip_db_upgrade: Skip database schema upgrade on startup.
    /// - cache_size: Cache size in bytes for the database page cache.
    pub async fn open(
        path: impl AsRef<Path>,
        metrics: DbMetrics,
        db_upgrade: DbUpgrade,
        cache_size: ByteSize,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref().to_owned();
        tokio::task::spawn_blocking(move || {
            let db = Db::new(path, metrics, db_upgrade, cache_size)?;
            db.create_tables()?;
            Ok(Self { db: Arc::new(db) })
        })
        .await?
    }

    /// Spawn a background task to monitor the database size at regular intervals.
    pub fn spawn_monitor(&self, interval: Duration) -> JoinHandle<()> {
        self.db.spawn_monitor(interval)
    }

    /// Get the minimum height in the DB.
    pub async fn min_height(&self) -> Result<Option<Height>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.min_height()).await?
    }

    /// Get the maximum height in the DB.
    pub async fn max_height(&self) -> Result<Option<Height>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.max_height()).await?
    }

    /// Get the execution layer payload for the given height.
    /// - height: The height to get the EL payload for.
    pub async fn get_payload(
        &self,
        height: Height,
    ) -> Result<Option<ExecutionPayloadV3>, StoreError> {
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || db.get_payload(height)).await?
    }

    /// Get the commit certificate for the given height.
    /// - height: The height to get the commit certificate for. If None, get the latest certificate.
    pub async fn get_certificate(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredCommitCertificate>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_certificate(height)).await?
    }

    /// Get the decided block for the given height.
    /// - height: The height to get the decided block for.
    pub async fn get_decided_block(
        &self,
        height: Height,
    ) -> Result<Option<DecidedBlock>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_decided_block(height)).await?
    }

    /// Store the decided block.
    /// - certificate: The certificate for the decided value.
    /// - block: The block to store.
    pub async fn store_decided_block(
        &self,
        certificate: CommitCertificate<ArcContext>,
        execution_payload: ExecutionPayloadV3,
        proposer: Address,
    ) -> Result<(), StoreError> {
        let decided_block = DecidedBlock::new(execution_payload, certificate);

        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.insert_decided_block(decided_block, proposer))
            .await?
    }

    /// Extend an existing commit certificate with additional precommit signatures collected during finalization.
    /// - certificate: The extended certificate to store.
    ///   Must have the same height as the existing certificate and a superset of commit signatures.
    pub async fn extend_certificate(
        &self,
        certificate: CommitCertificate<ArcContext>,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.extend_certificate(certificate)).await?
    }

    /// Store misbehavior evidence for a given height.
    /// - evidence: The misbehavior evidence to store.
    pub async fn store_misbehavior_evidence(
        &self,
        evidence: StoredMisbehaviorEvidence,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.insert_misbehavior_evidence(evidence)).await?
    }

    /// Get the misbehavior evidence for the given height.
    /// - height: The height to get the misbehavior evidence for. If None, use the latest height.
    ///
    /// Returns:
    /// - `Some(evidence)` with actual or empty evidence for finalized heights
    /// - `None` if the requested height was not yet finalized
    pub async fn get_misbehavior_evidence(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredMisbehaviorEvidence>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_misbehavior_evidence(height)).await?
    }

    /// Store proposal monitor data for a given height.
    /// - data: The proposal monitor data to store.
    pub async fn store_proposal_monitor_data(
        &self,
        data: ProposalMonitor,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.insert_proposal_monitor_data(data)).await?
    }

    /// Get the proposal monitor data for the given height.
    /// - height: The height to get the data for. If None, get the latest.
    pub async fn get_proposal_monitor_data(
        &self,
        height: Option<Height>,
    ) -> Result<Option<ProposalMonitor>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_proposal_monitor_data(height)).await?
    }

    /// Get invalid payloads for the given height.
    ///
    /// - height: The height to get the invalid payloads for. If None, use the latest height.
    pub async fn get_invalid_payloads(
        &self,
        height: Option<Height>,
    ) -> Result<Option<StoredInvalidPayloads>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_invalid_payloads(height)).await?
    }

    /// Appends an invalid payload to the stored collection for its height, creating
    /// the collection if none exists yet.
    ///
    /// The underlying read-modify-write runs inside a single
    /// redb write transaction, so concurrent calls for the
    /// same height are serialised by the database and cannot
    /// lose data.
    pub async fn append_invalid_payload(
        &self,
        invalid_payload: InvalidPayload,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.append_invalid_payload(invalid_payload)).await?
    }

    /// Store the undecided block.
    /// - block: The block to store.
    pub async fn store_undecided_block(&self, block: ConsensusBlock) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let mut tx = db.db.begin_write()?;
            db.insert_undecided_block(&mut tx, block)?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Get the undecided block for the given height and round.
    /// - height: The height to get the undecided block for.
    /// - round: The round to get the undecided block for.
    pub async fn get_undecided_block(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, StoreError> {
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || db.get_undecided_block(height, round, block_hash))
            .await?
    }

    /// Get the undecided block for the given height and block hash (ignoring round).
    /// Returns the first undecided block found that matches the height and block hash.
    /// - height: The height to get the undecided block for.
    /// - block_hash: The block hash to get the undecided block for.
    pub async fn get_undecided_block_by_height_and_block_hash(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, StoreError> {
        let db = Arc::clone(&self.db);

        tokio::task::spawn_blocking(move || {
            db.get_undecided_block_by_height_and_block_hash(height, block_hash)
        })
        .await?
    }

    /// Retrieves all undecided blocks for a given height and round.
    /// Called by the application when starting a new round and existing blocks need to be replayed.
    pub async fn get_undecided_blocks(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ConsensusBlock>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_undecided_blocks(height, round)).await?
    }

    /// Stores a pending proposal
    /// Called by the application when receiving new proposals from peers.
    ///
    /// ## Returns
    /// Whether or not the proposal was stored (may be rejected if limit is reached).
    pub async fn store_pending_proposal_parts(
        &self,
        value: ProposalParts,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<bool, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            db.insert_pending_proposal_parts(value, max_pending_proposals, current_height)
        })
        .await?
    }

    /// Enforce limits on the pending proposals table.
    ///
    /// Called at application at startup to clean up any excess proposals from previous runs.
    /// Uses the provided current_height to determine which proposals to keep.
    ///
    /// ## Returns
    /// A list of removed proposals, identified by their height, round, and block_hash.
    pub async fn enforce_pending_proposals_limit(
        &self,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<Vec<(Height, Round, BlockHash)>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            db.enforce_pending_proposals_limit(max_pending_proposals, current_height)
        })
        .await?
    }

    /// Retrieves all pending proposal parts for a given height and round.
    /// Called by the application when starting a new round and existing proposals need to be replayed.
    pub async fn get_pending_proposal_parts(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ProposalParts>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_pending_proposal_parts(height, round)).await?
    }

    /// Return the total number of stored pending proposal parts.
    pub async fn get_pending_proposal_parts_count(&self) -> Result<usize, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_pending_proposal_parts_count()).await?
    }

    /// Return the number of stored pending proposal parts grouped by height.
    /// Each entry in the returned vector contains the height and the count of
    /// proposal parts currently stored in `PENDING_PROPOSAL_PARTS_TABLE` for that
    /// height.
    pub async fn get_pending_proposal_parts_counts(
        &self,
    ) -> Result<Vec<(Height, usize)>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.get_pending_proposal_parts_counts()).await?
    }

    /// Atomically removes pending proposal parts and stores the undecided block.
    /// This ensures that if the process fails, the parts are not lost.
    pub async fn remove_pending_parts_and_store_undecided_block(
        &self,
        parts: ProposalParts,
        block: ConsensusBlock,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || {
            let mut tx = db.db.begin_write()?;
            db.remove_pending_proposal_parts(&mut tx, parts)?;
            db.insert_undecided_block(&mut tx, block)?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }

    /// Clean up stale consensus data (undecided blocks and pending proposals) for committed heights.
    /// Should always be called when committing a block, regardless of pruning configuration.
    /// - current_height: All undecided/pending data with height <= current_height will be removed
    pub async fn clean_stale_consensus_data(
        &self,
        current_height: Height,
    ) -> Result<(), StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.clean_stale_consensus_data(current_height)).await?
    }

    /// Prune historical certificates.
    /// Should only be called when pruning is enabled.
    /// - retain_height: The minimum height to retain for certificates.
    pub async fn prune_historical_certs(
        &self,
        retain_height: Height,
    ) -> Result<Vec<Height>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.prune_historical_certs(retain_height)).await?
    }

    /// Prune decided blocks.
    /// Should be called regardless of whether pruning is enabled.
    /// As historical decided blocks are fetched from EL, we just keep a minimum number of blocks
    /// in the DB to help with EL's amnesia upon recovery.
    pub async fn prune_blocks(&self) -> Result<Vec<Height>, StoreError> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || db.prune_blocks()).await?
    }

    /// Create a savepoint in the database to ensure the allocator state table is up to date.
    /// Doing this before shutting down the database can help avoid repair on next startup.
    pub fn savepoint(&self) {
        info!("Creating database savepoint...");
        self.db.savepoint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_rpc_types_engine::ExecutionPayloadV3;
    use arbitrary::Unstructured;
    use arc_consensus_types::signing::Signature;
    use arc_consensus_types::{
        Address, CommitSignature, ProposalData, ProposalFin, ProposalInit, ProposalPart, ValueId,
        Vote,
    };
    use bytes::Bytes;
    use malachitebft_app_channel::app::types::core::Validity;
    use tempfile::tempdir;

    const TEST_CACHE_SIZE: ByteSize = ByteSize::mib(64);

    async fn create_store() -> Store {
        let dir = tempdir().unwrap();
        Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            TEST_CACHE_SIZE,
        )
        .await
        .unwrap()
    }

    async fn create_test_proposal_parts(
        height: Height,
        round: Round,
        proposer: Address,
    ) -> ProposalParts {
        // Create a dummy signature for testing
        let signature = Signature::from_bytes([0u8; 64]);

        let parts = vec![
            ProposalPart::Init(ProposalInit::new(height, round, Round::Nil, proposer)),
            ProposalPart::Data(ProposalData::new(Bytes::from_static(b"test data"))),
            ProposalPart::Fin(ProposalFin::new(signature)),
        ];

        ProposalParts::new(parts).unwrap()
    }

    fn arbitrary_payload() -> ExecutionPayloadV3 {
        Unstructured::new(&[0xab; 1024])
            .arbitrary::<ExecutionPayloadV3>()
            .unwrap()
    }

    #[tokio::test]
    async fn test_store_and_get_decided_block() {
        let store = create_store().await;

        let height = Height::new(1);
        let round = Round::new(0);
        let payload = arbitrary_payload();
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_id = ValueId::new(block_hash);
        let cert = CommitCertificate::<ArcContext>::new(height, round, value_id, vec![]);
        let block = ConsensusBlock {
            height,
            round,
            valid_round: round,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: payload,
            signature: None,
        };

        store
            .store_decided_block(
                cert.clone(),
                block.execution_payload.clone(),
                block.proposer,
            )
            .await
            .unwrap();

        let retrieved = store.get_decided_block(height).await.unwrap();

        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.execution_payload, block.execution_payload);
        assert_eq!(retrieved.certificate.height, cert.height);

        let retrieved_payload = store.get_payload(height).await.unwrap();
        assert!(retrieved_payload.is_some());
        let retrieved_payload = retrieved_payload.unwrap();
        assert_eq!(retrieved.execution_payload, retrieved_payload);
    }

    #[tokio::test]
    async fn test_store_extended_certificate() {
        use malachitebft_core_types::{NilOrVal, SignedMessage};

        let store = create_store().await;

        let height = Height::new(1);
        let round = Round::new(0);
        let payload = arbitrary_payload();
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_id = ValueId::new(block_hash);

        // Create signed precommits
        let signature = Signature::from_bytes([0xab; 64]);
        let addresses = [
            Address::new([1u8; 20]),
            Address::new([2u8; 20]),
            Address::new([3u8; 20]),
        ];
        let commits: Vec<_> = addresses
            .iter()
            .map(|addr| {
                let vote = Vote::new_precommit(height, round, NilOrVal::Val(value_id), *addr);
                SignedMessage::new(vote, signature)
            })
            .collect();

        let cert = CommitCertificate::<ArcContext>::new(height, round, value_id, commits);

        // First store a decided block to create the initial minimal certificate
        store
            .store_decided_block(cert.clone(), payload, Address::new([0u8; 20]))
            .await
            .unwrap();

        let mut stored = store.get_certificate(Some(height)).await.unwrap().unwrap();

        assert_eq!(stored.certificate_type, CommitCertificateType::Minimal);
        assert_eq!(stored.certificate.commit_signatures.len(), 3);

        // Add one more signature to the certificate to simulate extension during finalization
        let additional_commit_signature = {
            let signature = Signature::from_bytes([0xcd; 64]);
            let address = Address::new([4u8; 20]);
            CommitSignature::new(address, signature)
        };
        stored
            .certificate
            .commit_signatures
            .push(additional_commit_signature);

        // Store the extended certificate
        store
            .extend_certificate(stored.certificate.clone())
            .await
            .unwrap();

        // Retrieve and verify
        let retrieved = store.get_certificate(Some(height)).await.unwrap().unwrap();
        assert_eq!(retrieved.certificate_type, CommitCertificateType::Extended);
        assert_eq!(retrieved.certificate.height, cert.height);
        assert_eq!(retrieved.certificate.round, cert.round);
        assert_eq!(retrieved.certificate.value_id, cert.value_id);
        assert_eq!(retrieved.certificate.commit_signatures.len(), 4);
    }

    #[tokio::test]
    async fn test_extend_certificate_without_existing_fails() {
        let store = create_store().await;
        let height = Height::new(1);
        let round = Round::new(0);
        let value_id = ValueId::new(B256::random());
        let cert = CommitCertificate::<ArcContext>::new(height, round, value_id, vec![]);

        let result = store.extend_certificate(cert).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_certificate_not_found() {
        let store = create_store().await;

        // Height was never stored
        let result = store.get_certificate(Some(Height::new(999))).await.unwrap();
        assert!(result.is_none());

        let result = store.get_certificate(None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_store_misbehavior_evidence() {
        use arc_consensus_types::evidence::{
            DoubleVote, StoredMisbehaviorEvidence, ValidatorEvidence,
        };
        use malachitebft_core_types::{NilOrVal, SignedMessage};

        let store = create_store().await;

        let height = Height::new(5);
        let round = Round::new(0);
        let signature = Signature::from_bytes([0xab; 64]);
        let address = Address::new([1u8; 20]);

        // Store a block at the height first (to establish max_height)
        store_block_at_height(&store, height).await;

        // Create two conflicting signed votes
        let value_id_1 = ValueId::new(BlockHash::repeat_byte(0x11));
        let value_id_2 = ValueId::new(BlockHash::repeat_byte(0x22));

        let vote1 = Vote::new_precommit(height, round, NilOrVal::Val(value_id_1), address);
        let vote2 = Vote::new_precommit(height, round, NilOrVal::Val(value_id_2), address);

        let signed_vote1 = SignedMessage::new(vote1, signature);
        let signed_vote2 = SignedMessage::new(vote2, signature);

        let double_vote = DoubleVote {
            first: signed_vote1,
            second: signed_vote2,
        };

        let validator_evidence = ValidatorEvidence {
            address,
            double_votes: vec![double_vote],
            double_proposals: vec![],
        };

        let evidence = StoredMisbehaviorEvidence {
            height,
            validators: vec![validator_evidence],
        };

        // Store the evidence
        store
            .store_misbehavior_evidence(evidence.clone())
            .await
            .unwrap();

        // Retrieve and verify
        let retrieved = store
            .get_misbehavior_evidence(Some(height))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(retrieved.height, evidence.height);
        assert_eq!(retrieved.validators.len(), 1);
        assert_eq!(retrieved.validators[0].address, address);
        assert_eq!(retrieved.validators[0].double_votes.len(), 1);
        assert!(retrieved.validators[0].double_proposals.is_empty());
    }

    #[tokio::test]
    async fn test_get_misbehavior_evidence_not_found() {
        let store = create_store().await;

        // Height was never stored
        let result = store
            .get_misbehavior_evidence(Some(Height::new(9999)))
            .await
            .unwrap();
        assert!(result.is_none());

        let result = store.get_misbehavior_evidence(None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_store_and_get_undecided_block() {
        let store = create_store().await;

        let height = Height::new(2);
        let round = Round::new(1);
        let block = ConsensusBlock {
            height,
            round,
            valid_round: round,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: arbitrary_payload(),
            signature: None,
        };

        store.store_undecided_block(block.clone()).await.unwrap();

        let retrieved = store
            .get_undecided_block(height, round, block.block_hash())
            .await
            .unwrap();

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), block);
    }

    #[tokio::test]
    async fn test_pending_proposal_parts_counts() {
        // no proposal parts
        let store = create_store().await;
        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        assert!(counts.is_empty());

        // proposal parts exist in multiple heights
        let height_a = Height::new(3);
        for r in 0..3 {
            let round = Round::new(r);
            let proposer = Address::new([r as u8; 20]);
            let parts = create_test_proposal_parts(height_a, round, proposer).await;
            let inserted = store
                .store_pending_proposal_parts(parts, 100, Height::new(1))
                .await
                .unwrap();

            assert!(inserted, "Proposal parts should be inserted successfully");
        }

        let height_b = Height::new(7);
        for r in 0..2 {
            let round = Round::new(10 + r);
            let proposer = Address::new([(10 + r) as u8; 20]);
            let parts = create_test_proposal_parts(height_b, round, proposer).await;
            let inserted = store
                .store_pending_proposal_parts(parts, 100, Height::new(1))
                .await
                .unwrap();

            assert!(inserted, "Proposal parts should be inserted successfully");
        }

        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        assert_eq!(counts, vec![(height_a, 3), (height_b, 2)]);
    }

    #[tokio::test]
    async fn test_prune_historical_certs() {
        let store = create_store().await;

        // Insert dummy values at heights 1 to 4.
        store_blocks_at_heights(&store, &[1, 2, 3, 4]).await;

        let pruned = store.prune_historical_certs(Height::new(3)).await.unwrap();
        assert_eq!(pruned.len(), 2);
        assert_eq!(pruned, vec![Height::new(1), Height::new(2)]);

        // Verify certificates are pruned but blocks remain.
        for h in 1..=4 {
            let height = Height::new(h);
            let cert_exists = store.get_certificate(Some(height)).await.unwrap().is_some();
            let block_exists = store.get_payload(height).await.unwrap().is_some();

            if h < 3 {
                assert!(!cert_exists, "Certificate at height {} should be pruned", h);
                assert!(block_exists, "Block at height {} should still exist", h);
            } else {
                assert!(
                    cert_exists,
                    "Certificate at height {} should be retained",
                    h
                );
                assert!(block_exists, "Block at height {} should still exist", h);
            }
        }

        assert_eq!(store.min_height().await.unwrap().unwrap().as_u64(), 3);
        assert_eq!(store.max_height().await.unwrap().unwrap().as_u64(), 4);
    }

    #[tokio::test]
    async fn test_prune_certs_then_blocks() {
        let store = create_store().await;

        let retain_height = 50u64;
        let heights_to_insert = vec![1, 29, 30, 49, 50, 51, 129, 130];
        let max_height = heights_to_insert.iter().max().unwrap().to_owned();
        let heights_to_retain = heights_to_insert
            .iter()
            .filter(|&h| *h >= retain_height)
            .map(|&h| Height::new(h))
            .collect::<Vec<_>>();
        let heights_to_prune = heights_to_insert
            .iter()
            .filter(|&h| *h < retain_height)
            .map(|&h| Height::new(h))
            .collect::<Vec<_>>();

        store_blocks_at_heights(&store, &heights_to_insert).await;

        // Verify all blocks exist before pruning.
        for h in heights_to_insert.iter() {
            let height = Height::new(*h);
            assert!(
                store.get_payload(height).await.unwrap().is_some(),
                "Block at height {} should exist before pruning",
                h
            );
            assert!(
                store.get_certificate(Some(height)).await.unwrap().is_some(),
                "Certificate at height {} should exist before pruning",
                h
            );
        }

        // Prune historical certificates up to retain_height.
        let pruned_certs = store
            .prune_historical_certs(Height::new(retain_height))
            .await
            .unwrap();
        assert_eq!(pruned_certs, heights_to_prune);

        // After cert pruning, blocks are still there.
        for h in heights_to_prune.iter() {
            assert!(
                store.get_certificate(Some(*h)).await.unwrap().is_none(),
                "Certificate at height {} should be pruned (< retain_height)",
                h
            );
            assert!(
                store.get_payload(*h).await.unwrap().is_some(),
                "Block at height {} should still exist after cert pruning",
                h
            );
        }
        for h in heights_to_retain.iter() {
            assert!(
                store.get_certificate(Some(*h)).await.unwrap().is_some(),
                "Certificate at height {} should be retained (>= retain_height)",
                h
            );
            assert!(
                store.get_payload(*h).await.unwrap().is_some(),
                "Block at height {} should still exist",
                h
            );
        }

        assert_eq!(
            store.min_height().await.unwrap().unwrap().as_u64(),
            retain_height
        );
        assert_eq!(
            store.max_height().await.unwrap().unwrap().as_u64(),
            max_height
        );

        let amnesia_retain_height = heights_to_insert
            .iter()
            .max()
            .unwrap()
            .to_owned()
            .saturating_sub(Db::RETH_AMNESIA_HEIGHT_COUNT);
        let amn_heights_to_retain: Vec<Height> = heights_to_insert
            .iter()
            .filter(|&h| *h >= amnesia_retain_height)
            .map(|&h| Height::new(h))
            .collect();
        let amn_heights_to_prune: Vec<Height> = heights_to_insert
            .iter()
            .filter(|&h| *h < amnesia_retain_height)
            .map(|&h| Height::new(h))
            .collect();

        let pruned_blocks = store.prune_blocks().await.unwrap();
        assert_eq!(pruned_blocks, amn_heights_to_prune);

        // Verify blocks below 30 are pruned; 30 and above retained.
        for h in amn_heights_to_prune.iter() {
            assert!(
                store.get_payload(*h).await.unwrap().is_none(),
                "Block at height {} should be pruned (< 30)",
                h
            );
        }
        for h in amn_heights_to_retain.iter() {
            assert!(
                store.get_payload(*h).await.unwrap().is_some(),
                "Block at height {} should be retained (>= 30)",
                h
            );
        }

        // Certificates still define bounds.
        assert_eq!(
            store.min_height().await.unwrap().unwrap().as_u64(),
            retain_height
        );
        assert_eq!(
            store.max_height().await.unwrap().unwrap().as_u64(),
            max_height
        );
    }

    #[tokio::test]
    async fn test_prune_blocks() {
        let store = create_store().await;

        let retain_height = 50u64;
        let max_height = retain_height + Db::RETH_AMNESIA_HEIGHT_COUNT;

        let heights_to_insert = vec![
            retain_height.saturating_sub(2),
            retain_height.saturating_sub(1),
            retain_height,
            retain_height + 1,
            retain_height + 10,
            max_height,
        ];

        let heights_to_retain = heights_to_insert
            .iter()
            .filter(|&h| *h >= retain_height)
            .map(|&h| Height::new(h))
            .collect::<Vec<_>>();
        let heights_to_prune = heights_to_insert
            .iter()
            .filter(|&h| *h < retain_height)
            .map(|&h| Height::new(h))
            .collect::<Vec<_>>();

        store_blocks_at_heights(&store, &heights_to_insert).await;

        // Verify all blocks exist before pruning.
        for &h in heights_to_insert.iter() {
            let height = Height::new(h);
            assert!(
                store.get_payload(height).await.unwrap().is_some(),
                "Block at height {} should exist before pruning",
                h
            );
        }

        let pruned = store.prune_blocks().await.unwrap();

        assert_eq!(pruned.len(), heights_to_prune.len(),
            "Expected {} blocks to be pruned, but {} were pruned. retain_height={}, RETH_AMNESIA_HEIGHT_COUNT={}",
            heights_to_prune.len(), pruned.len(), retain_height, Db::RETH_AMNESIA_HEIGHT_COUNT);
        assert_eq!(pruned, heights_to_prune);

        for height in heights_to_prune.iter() {
            assert!(
                store.get_payload(*height).await.unwrap().is_none(),
                "Block at height {} should be pruned (< retain_height {})",
                height,
                retain_height,
            );
        }

        for height in heights_to_retain.iter() {
            assert!(
                store.get_payload(*height).await.unwrap().is_some(),
                "Block at height {} should be retained (>= retain_height {})",
                height,
                retain_height,
            );
        }

        assert_eq!(
            store.min_height().await.unwrap().unwrap().as_u64(),
            *heights_to_insert.iter().min().unwrap() // min height is based on certificates, which are not pruned here
        );
        assert_eq!(
            store.max_height().await.unwrap().unwrap(),
            Height::new(max_height)
        );
    }

    #[tokio::test]
    async fn test_prune_blocks_with_empty_store() {
        let store = create_store().await;

        let pruned = store.prune_blocks().await.unwrap();
        assert!(pruned.is_empty());

        assert_eq!(store.min_height().await.unwrap(), None);
        assert_eq!(store.max_height().await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_prune_blocks_with_few_blocks() {
        let store = create_store().await;

        let max_test_height = Db::RETH_AMNESIA_HEIGHT_COUNT / 2;

        let heights = (1..=max_test_height).collect::<Vec<_>>();
        store_blocks_at_heights(&store, &heights).await;

        let pruned = store.prune_blocks().await.unwrap();
        assert!(
            pruned.is_empty(),
            "No blocks should be pruned when max_height < RETH_AMNESIA_HEIGHT_COUNT (max_height={}, RETH_AMNESIA_HEIGHT_COUNT={})",
            max_test_height, Db::RETH_AMNESIA_HEIGHT_COUNT
        );

        for h in 1..=max_test_height {
            let height = Height::new(h);
            assert!(
                store.get_payload(height).await.unwrap().is_some(),
                "Block at height {} should be retained",
                h
            );
        }
    }

    #[tokio::test]
    async fn test_prune_historical_certs_batch_cap() {
        let store = create_store().await;

        let limit = Db::PRUNE_BATCH_LIMIT as u64;
        let retain_height = limit + 50; // more than the limit
        let curr_height = retain_height + 2;

        let mut heights = Vec::new();
        heights.extend(1..=curr_height); // insert some above retain_height

        store_blocks_at_heights(&store, &heights).await;

        let pruned1 = store
            .prune_historical_certs(Height::new(retain_height))
            .await
            .unwrap();
        assert_eq!(pruned1.len() as u64, limit);
        let expected1 = (1..=limit).map(Height::new).collect::<Vec<_>>();
        assert_eq!(expected1, pruned1);

        let pruned2 = store
            .prune_historical_certs(Height::new(retain_height))
            .await
            .unwrap();
        assert_eq!(pruned2.len() as u64, retain_height - 1 - limit);
        let expected2 = ((limit + 1)..retain_height)
            .map(Height::new)
            .collect::<Vec<_>>();
        assert_eq!(pruned2, expected2);

        assert_eq!(
            store.min_height().await.unwrap().unwrap(),
            Height::new(retain_height)
        );
        assert_eq!(
            store.max_height().await.unwrap().unwrap(),
            Height::new(retain_height + 2)
        );

        for h in 1..retain_height {
            let height = Height::new(h);
            assert!(
                store.get_payload(height).await.unwrap().is_some(),
                "Block at height {} should still exist after cert pruning",
                h
            );
        }
    }

    #[tokio::test]
    async fn test_prune_blocks_batch_cap() {
        let store = create_store().await;

        let limit = Db::PRUNE_BATCH_LIMIT as u64;
        let curr_height = limit + Db::RETH_AMNESIA_HEIGHT_COUNT + 10; // more than one batch
        let retain_height = curr_height - Db::RETH_AMNESIA_HEIGHT_COUNT;

        let mut heights = Vec::new();
        heights.extend(1..=curr_height);

        store_blocks_at_heights(&store, &heights).await;

        let pruned1 = store.prune_blocks().await.unwrap();
        assert_eq!(pruned1.len() as u64, limit);
        let expected1 = (1..=limit).map(Height::new).collect::<Vec<_>>();
        assert_eq!(pruned1, expected1);

        let pruned2 = store.prune_blocks().await.unwrap();
        assert_eq!(pruned2.len() as u64, retain_height - 1 - limit);
        let expected2 = ((limit + 1)..retain_height)
            .map(Height::new)
            .collect::<Vec<_>>();
        assert_eq!(pruned2, expected2);

        for h in 1..retain_height {
            let height = Height::new(h);
            assert!(
                store.get_payload(height).await.unwrap().is_none(),
                "Block at height {} should be pruned (< retain_height {})",
                h,
                retain_height
            );
        }
        for h in retain_height..=curr_height {
            let height = Height::new(h);
            assert!(
                store.get_payload(height).await.unwrap().is_some(),
                "Block at height {} should be retained (>= retain_height {})",
                h,
                retain_height
            );
        }

        // Certificates still define bounds, unchanged by block pruning
        assert_eq!(store.min_height().await.unwrap().unwrap(), Height::new(1));
        assert_eq!(
            store.max_height().await.unwrap().unwrap(),
            Height::new(curr_height)
        );
    }

    async fn store_block_at_height(store: &Store, height: Height) {
        let round = Round::new(0);
        let payload = arbitrary_payload();
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_id = ValueId::new(block_hash);
        let cert = CommitCertificate::<ArcContext>::new(height, round, value_id, vec![]);
        let block = ConsensusBlock {
            height,
            round,
            valid_round: round,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: payload,
            signature: None,
        };
        store
            .store_decided_block(cert, block.execution_payload, block.proposer)
            .await
            .unwrap();
    }

    /// Helper function to store blocks at multiple heights
    async fn store_blocks_at_heights(store: &Store, heights: &[u64]) {
        for &h in heights {
            store_block_at_height(store, Height::new(h)).await;
        }
    }

    #[tokio::test]
    async fn test_min_decided_value_height() {
        let store = create_store().await;

        let heights = [Height::new(42), Height::new(10), Height::new(99)];

        for &h in &heights {
            store_block_at_height(&store, h).await;
        }

        let min_height = store.min_height().await;
        assert!(min_height.is_ok());
        assert_eq!(min_height.unwrap(), Some(Height::new(10)));
    }

    #[tokio::test]
    async fn test_max_decided_value_height() {
        let store = create_store().await;

        let heights = [Height::new(2), Height::new(100), Height::new(99)];

        for &h in &heights {
            store_block_at_height(&store, h).await;
        }

        let max_height = store.max_height().await;
        assert!(max_height.is_ok());
        assert_eq!(max_height.unwrap(), Some(Height::new(100)));
    }

    #[tokio::test]
    async fn test_clean_stale_consensus_data() {
        let store = create_store().await;

        // Create undecided blocks at height 2 for rounds 1 and 2
        let height = Height::new(2);
        let payload = arbitrary_payload();
        for r in 1..=2 {
            let round = Round::new(r as u32);
            let block = ConsensusBlock {
                height,
                round,
                valid_round: Round::Nil,
                proposer: Address::new([r; 20]),
                validity: Validity::Valid,
                execution_payload: payload.clone(),
                signature: None,
            };
            store.store_undecided_block(block).await.unwrap();

            // Check undecided blocks exist
            let undecided_blocks = store.get_undecided_blocks(height, round).await.unwrap();
            assert!(
                undecided_blocks.len() == 1,
                "1 undecided block should exist at height {} and round {}",
                height,
                r
            );
        }

        let height = Height::new(3);
        // Create two pending proposal parts at height 3 for rounds 1 and 2
        for p in 1..=2 {
            let proposer = Address::new([p; 20]);
            let round = Round::new(p as u32);
            let parts = create_test_proposal_parts(height, round, proposer).await;
            let inserted = store
                .store_pending_proposal_parts(parts, 100, Height::new(1))
                .await
                .unwrap();
            assert!(
                inserted,
                "Proposal parts should be inserted successfully at height {} and round {}",
                height, round
            );

            // Check pending parts exist
            let pending_parts = store
                .get_pending_proposal_parts(height, round)
                .await
                .unwrap();
            assert!(
                pending_parts.len() == 1,
                "1 pending part should exist at height {} and round {}",
                height,
                round
            );
        }

        // Clean stale data up to height 2
        // Simulates decision at height 2, undecided blocks at height 2 should be removed, pending parts at height 3 should be kept
        store
            .clean_stale_consensus_data(Height::new(2))
            .await
            .unwrap();

        // Check undecided blocks are removed
        for r in 1..=2 {
            let undecided_blocks = store
                .get_undecided_blocks(Height::new(2), Round::new(r as u32))
                .await
                .unwrap();
            assert!(
                undecided_blocks.is_empty(),
                "Undecided block should be removed at height {} and round {}",
                height,
                r
            );
        }

        // Check pending parts are kept
        for r in 1..=2 {
            let pending_parts = store
                .get_pending_proposal_parts(Height::new(3), Round::new(r as u32))
                .await
                .unwrap();
            assert!(
                pending_parts.len() == 1,
                "1 pending part should exist at height {} and round {}",
                height,
                r
            );
        }
    }

    async fn test_enforce_pending_limit_on_insert(
        max_pending: usize,
        current_height: u64,
        proposals_to_insert: Vec<(u64, u32)>,
        proposal_to_reject: (u64, u32),
    ) {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            TEST_CACHE_SIZE,
        )
        .await
        .unwrap();

        let current_height = Height::new(current_height);
        let proposer = Address::new([1; 20]);

        // Fill the table with proposals
        for (h, r) in &proposals_to_insert {
            let height = Height::new(*h);
            let round = Round::new(*r);
            let parts = create_test_proposal_parts(height, round, proposer).await;
            let inserted = store
                .store_pending_proposal_parts(parts, max_pending, current_height)
                .await
                .unwrap();
            assert!(
                inserted,
                "Proposal at height {h} and round {r} should be inserted",
            );
        }

        // Verify we have max_pending entries
        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        assert_eq!(
            total, max_pending,
            "Should have exactly {} entries",
            max_pending
        );

        // Try to add one more proposal (should be rejected)
        let height_reject = Height::new(proposal_to_reject.0);
        let round_reject = Round::new(proposal_to_reject.1);
        let parts_reject = create_test_proposal_parts(height_reject, round_reject, proposer).await;
        let inserted = store
            .store_pending_proposal_parts(parts_reject, max_pending, current_height)
            .await
            .unwrap();
        assert!(
            !inserted,
            "Proposal at height {} and round {} should be rejected",
            proposal_to_reject.0, proposal_to_reject.1
        );

        // Verify the extra proposal was NOT added (still max_pending entries)
        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        assert_eq!(
            total, max_pending,
            "Should still have exactly {} entries, extra was rejected",
            max_pending
        );

        // Verify that the rejected proposal is not in the table
        let parts_at_reject = store
            .get_pending_proposal_parts(height_reject, round_reject)
            .await
            .unwrap();
        assert_eq!(
            parts_at_reject.len(),
            0,
            "Proposal at height {} and round {} should not be stored",
            proposal_to_reject.0,
            proposal_to_reject.1
        );

        // Verify all original proposals are still there
        for (h, r) in &proposals_to_insert {
            let height = Height::new(*h);
            let round = Round::new(*r);
            let parts = store
                .get_pending_proposal_parts(height, round)
                .await
                .unwrap();
            assert_eq!(
                parts.len(),
                1,
                "Proposal at height {h} and round {r} should be stored",
            );
        }

        drop(store);
        dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_enforce_pending_limit_on_insert_cases() {
        // Test with new proposal at height further in the future than max_pending
        test_enforce_pending_limit_on_insert(
            4,
            10,
            vec![(10, 0), (11, 0), (12, 0), (13, 0)],
            (14, 0),
        )
        .await;

        // Test with new proposal at height within max_pending range, rejected due to multiple proposals at the same height
        test_enforce_pending_limit_on_insert(
            4,
            20,
            vec![(20, 0), (20, 1), (21, 0), (22, 0)],
            (23, 0),
        )
        .await;
    }

    async fn test_enforce_pending_limit_on_startup(
        initial_proposals: Vec<(u64, u32)>,
        current_height: u64,
        max_pending: usize,
        expected_remaining: Vec<(u64, u32)>,
    ) {
        let dir = tempdir().unwrap();
        let store = Store::open(
            dir.path().join("db"),
            DbMetrics::default(),
            DbUpgrade::Skip,
            TEST_CACHE_SIZE,
        )
        .await
        .unwrap();

        let proposer = Address::new([1; 20]);

        // Insert initial proposals (without any limit enforcement)
        for (h, r) in &initial_proposals {
            let height = Height::new(*h);
            let round = Round::new(*r);
            let parts = create_test_proposal_parts(height, round, proposer).await;
            // Use a very high max_pending to bypass limit during insertion
            let inserted = store
                .store_pending_proposal_parts(parts, 1000, Height::new(0))
                .await
                .unwrap();
            assert!(
                inserted,
                "Proposal at height {h} and round {r} should be inserted",
            );
        }

        // Verify all proposals were inserted
        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        assert_eq!(
            total,
            initial_proposals.len(),
            "Should have {} initial entries",
            initial_proposals.len()
        );

        // Enforce limit on startup
        let current_height = Height::new(current_height);
        let removed = store
            .enforce_pending_proposals_limit(max_pending, current_height)
            .await
            .unwrap();

        // Verify the number of removed proposals matches expectation
        let expected_removed_count = initial_proposals.len() - expected_remaining.len();
        assert_eq!(
            removed.len(),
            expected_removed_count,
            "Should have removed {} entries",
            expected_removed_count
        );

        // Verify the correct number of proposals remain
        let counts = store.get_pending_proposal_parts_counts().await.unwrap();
        let total: usize = counts.iter().map(|(_, count)| count).sum();
        assert_eq!(
            total,
            expected_remaining.len(),
            "Should have {} entries after cleanup",
            expected_remaining.len()
        );

        // Verify that expected proposals are present
        for (h, r) in &expected_remaining {
            let height = Height::new(*h);
            let round = Round::new(*r);
            let parts = store
                .get_pending_proposal_parts(height, round)
                .await
                .unwrap();
            assert_eq!(
                parts.len(),
                1,
                "Expected proposal at height {} round {} should be present",
                h,
                r
            );
        }

        // Verify that non-expected proposals are absent
        for (h, r) in &initial_proposals {
            if !expected_remaining.contains(&(*h, *r)) {
                let height = Height::new(*h);
                let round = Round::new(*r);
                let parts = store
                    .get_pending_proposal_parts(height, round)
                    .await
                    .unwrap();
                assert_eq!(
                    parts.len(),
                    0,
                    "Unexpected proposal at height {} round {} should be removed",
                    h,
                    r
                );
            }
        }

        drop(store);
        dir.close().unwrap();
    }

    #[tokio::test]
    async fn test_enforce_pending_limit_on_startup_cases() {
        // Remove stale entries (height < current_height)
        // current_height=10 means processing height 10
        // Stale: height < 10 (so 8, 9)
        // Keep (10, 0), (11, 0), (12, 0)
        test_enforce_pending_limit_on_startup(
            vec![(8, 0), (9, 0), (10, 0), (11, 0), (12, 0)],
            10,
            5,
            vec![(10, 0), (11, 0), (12, 0)],
        )
        .await;

        // Remove entries too far in the future
        // current_height=10, max_pending=3 allows to keep (10, 0), (11, 0), (12, 0)
        test_enforce_pending_limit_on_startup(
            vec![(10, 0), (11, 0), (12, 0), (13, 0), (20, 0)],
            10,
            3,
            vec![(10, 0), (11, 0), (12, 0)],
        )
        .await;

        // Trim to max_pending when within range but exceeds limit
        // current_height=10, max_pending=3 allows to keep (10, 0), (11, 0), (12, 0)
        test_enforce_pending_limit_on_startup(
            vec![(10, 0), (11, 0), (12, 0), (13, 0), (14, 0)],
            10,
            3,
            vec![(10, 0), (11, 0), (12, 0)],
        )
        .await;

        // Mixed scenario - stale, within range, and too far
        // current_height=10, max_pending=3 allows to keep (10, 0), (11, 0), (12, 0)
        test_enforce_pending_limit_on_startup(
            vec![
                (5, 0),  // stale (height < 10)
                (9, 0),  // stale (height < 10)
                (10, 0), // within range
                (11, 0), // within range
                (12, 0), // within range
                (13, 0), // too far (> 10 + 3 - 1 = 12)
                (20, 0), // too far
            ],
            10,
            3,
            vec![(10, 0), (11, 0), (12, 0)],
        )
        .await;

        // Same height, different rounds
        // current_height=10, max_pending=3 allows to keep (10, 0), (10, 1), (10, 2)
        test_enforce_pending_limit_on_startup(
            vec![(10, 0), (10, 1), (10, 2), (10, 3), (10, 4)],
            10,
            3,
            vec![(10, 0), (10, 1), (10, 2)],
        )
        .await;
    }
}
