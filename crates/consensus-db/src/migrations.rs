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

use std::time::{Duration, Instant};

use redb::{ReadableTable, TableDefinition};
use tracing::{debug, info, warn};

use crate::versions::SchemaVersion;

use crate::decoder::DecodeError;
use crate::store::{
    StoreError, CERTIFICATES_TABLE, DECIDED_BLOCKS_TABLE, PENDING_PROPOSAL_PARTS_TABLE,
    UNDECIDED_BLOCKS_TABLE,
};
use crate::versions::DB_SCHEMA_VERSION;

mod migrators;
use migrators::MigratorSet;

/// Metadata table to track database schema version
pub const METADATA_TABLE: TableDefinition<&str, SchemaVersion> = TableDefinition::new("_metadata");

const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Migration result with statistics
#[derive(Debug, Default)]
pub struct MigrationStats {
    pub tables_migrated: usize,
    pub records_scanned: usize,
    pub records_upgraded: usize,
    pub records_skipped: usize,
    pub duration: Duration,
}

impl MigrationStats {
    fn merge(&mut self, other: MigrationStats) {
        self.tables_migrated += other.tables_migrated;
        self.records_scanned += other.records_scanned;
        self.records_upgraded += other.records_upgraded;
        self.records_skipped += other.records_skipped;
    }
}

/// Trait for migrating a specific data type from one version to another
pub trait Migrator: Send + Sync {
    /// The name of this migrator (for logging)
    fn name(&self) -> &str;

    /// Source version
    fn source_version(&self) -> SchemaVersion;

    /// Target version
    fn target_version(&self) -> SchemaVersion;

    /// Migrate a single record from old format to new format
    fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError>;

    /// Check if this record needs migration
    fn needs_migration(&self, bytes: &[u8]) -> bool {
        bytes
            .first()
            .map(|&v| v == self.source_version().as_u8())
            .unwrap_or(false)
    }
}

/// Migration coordinator
pub struct MigrationCoordinator {
    db: redb::Database,
}

impl MigrationCoordinator {
    const BATCH_SIZE: usize = 1000;

    pub fn new(db: redb::Database) -> Self {
        Self { db }
    }

    pub fn into_db(self) -> redb::Database {
        self.db
    }

    /// Check current schema version
    pub fn current_schema_version(&self) -> Result<Option<SchemaVersion>, StoreError> {
        let tx = self.db.begin_read()?;
        let table = tx.open_table(METADATA_TABLE)?;
        Ok(table.get(SCHEMA_VERSION_KEY)?.map(|v| v.value()))
    }

    /// Set schema version
    fn set_schema_version(&self, version: SchemaVersion) -> Result<(), StoreError> {
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(METADATA_TABLE)?;
            table.insert(SCHEMA_VERSION_KEY, version)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Check if migration is needed
    ///
    /// Arguments:
    /// - db_exists: true if the database file exists, false if it doesn't
    ///
    /// Returns:
    /// - true if migration is needed, false if not
    /// - Err if the database version is unsupported
    pub fn needs_migration(&self, db_exists: bool) -> Result<bool, StoreError> {
        self.ensure_metadata_table_exists()?;

        let current = self.current_schema_version()?;
        let target = DB_SCHEMA_VERSION;

        match current {
            None if db_exists => {
                info!("Database has no schema version, setting to v0");
                self.set_schema_version(SchemaVersion::V0)?;
                Ok(true)
            }
            None => {
                info!("New database created, setting to current version {target}");
                self.set_schema_version(target)?;
                Ok(false)
            }
            Some(v) if v < target => {
                info!("Database schema {v} is outdated, current is {target}");
                Ok(true)
            }
            Some(v) if v > target => {
                warn!("Database schema {v} is newer than supported {target}");
                Err(StoreError::Decode(DecodeError::UnsupportedVersion(
                    v.as_u8(),
                )))
            }
            Some(_) => {
                debug!("Database schema is up to date");
                Ok(false)
            }
        }
    }

    /// Perform full migration
    ///
    /// Panics if the database schema version is not set.
    pub fn migrate(&self) -> Result<MigrationStats, StoreError> {
        let start = Instant::now();
        let mut stats = MigrationStats::default();

        let from_version = self
            .current_schema_version()?
            .expect("Database schema version should be set");
        let to_version = DB_SCHEMA_VERSION;

        info!(
            from = %from_version,
            to = %to_version,
            "Starting database migration"
        );

        // Build migration chain
        let migration_chain = self.build_migration_chain(from_version, to_version)?;

        if migration_chain.is_empty() {
            info!("No migrations needed");
            return Ok(stats);
        }

        // Migrate each table with appropriate strategy
        for (schema_version, migrators) in migration_chain {
            info!("Migrating to schema version {}", schema_version);

            stats.merge(self.migrate_certificates(migrators.certificate.as_ref())?);
            stats.merge(self.migrate_decided_blocks(migrators.decided_block.as_ref())?);
            stats.merge(self.migrate_undecided_blocks(migrators.undecided_block.as_ref())?);
            stats.merge(self.migrate_pending_parts(migrators.pending_parts.as_ref())?);

            // Update schema version after successful migration
            self.set_schema_version(schema_version)?;
            info!("Successfully migrated to schema version {}", schema_version);
        }

        stats.duration = start.elapsed();

        Ok(stats)
    }

    /// Ensure the metadata table exists by creating it if necessary
    fn ensure_metadata_table_exists(&self) -> Result<(), StoreError> {
        let tx = self.db.begin_write()?;
        {
            let _ = tx.open_table(METADATA_TABLE)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Build chain of migrations from source to target version
    fn build_migration_chain(
        &self,
        from: SchemaVersion,
        to: SchemaVersion,
    ) -> Result<Vec<(SchemaVersion, MigratorSet)>, StoreError> {
        let mut chain = Vec::new();

        for version in from.next().as_u8()..=to.as_u8() {
            let version = SchemaVersion::new(version);
            let migrators = self.get_migrators_for_version(version)?;
            chain.push((version, migrators));
        }

        Ok(chain)
    }

    /// Get migrators for a specific target version
    fn get_migrators_for_version(&self, version: SchemaVersion) -> Result<MigratorSet, StoreError> {
        match version {
            SchemaVersion::V1 => Ok(MigratorSet::v1()),
            // Future versions:
            // Version::V2 => Ok(MigratorSet::v2()),
            v => Err(StoreError::Decode(DecodeError::UnsupportedVersion(
                v.as_u8(),
            ))),
        }
    }

    /// Migrate certificates table.
    fn migrate_certificates(&self, migrator: &dyn Migrator) -> Result<MigrationStats, StoreError> {
        info!("Migrating certificates table");

        let mut stats = MigrationStats::default();

        // start from the min height
        let mut next_height = if let Some((min_height, _)) = self
            .db
            .begin_read()?
            .open_table(CERTIFICATES_TABLE)?
            .first()?
        {
            min_height.value()
        } else {
            // no records to migrate, return early
            return Ok(stats);
        };

        loop {
            let tx = self.db.begin_write()?;
            let mut records_scanned = 0;
            let records_upgraded;
            {
                let mut table = tx.open_table(CERTIFICATES_TABLE)?;

                // collect records to migrate in this batch
                let mut batch = Vec::with_capacity(Self::BATCH_SIZE);
                // iterate over BATCH_SIZE records
                for entry in table.range(next_height..)?.take(Self::BATCH_SIZE) {
                    let (key, value) = entry?;
                    let bytes = value.value();
                    records_scanned += 1;

                    if migrator.needs_migration(&bytes[..]) {
                        let new_bytes = migrator.migrate(&bytes[..]).map_err(|e| {
                            StoreError::Migration(format!(
                                "Failed to migrate certificate at height {}: {}",
                                key.value(),
                                e
                            ))
                        })?;

                        batch.push((key.value(), new_bytes));
                    }

                    next_height = key.value().increment();
                }

                // migrate the records in this batch
                records_upgraded = batch.len();
                for (key, value) in batch {
                    table.insert(key, value)?;
                }
            }
            tx.commit()?;
            debug!("migrated batch of {} records", records_upgraded);

            stats.records_scanned += records_scanned;
            stats.records_upgraded += records_upgraded;

            // if we scanned less than the batch size, we're done
            if records_scanned < Self::BATCH_SIZE {
                break;
            }
        }

        stats.records_skipped = stats.records_scanned - stats.records_upgraded;
        stats.tables_migrated += 1;

        info!(
            scanned = stats.records_scanned,
            upgraded = stats.records_upgraded,
            "Completed certificates migration"
        );

        Ok(stats)
    }

    /// Migrate decided blocks table.
    fn migrate_decided_blocks(
        &self,
        migrator: &dyn Migrator,
    ) -> Result<MigrationStats, StoreError> {
        info!("Migrating decided blocks table");

        let mut stats = MigrationStats::default();

        // start from the min height
        let mut next_height = if let Some((min_height, _)) = self
            .db
            .begin_read()?
            .open_table(DECIDED_BLOCKS_TABLE)?
            .first()?
        {
            min_height.value()
        } else {
            // no records to migrate, return early
            return Ok(stats);
        };

        loop {
            let tx = self.db.begin_write()?;
            let mut records_scanned = 0;
            let records_upgraded;
            {
                let mut table = tx.open_table(DECIDED_BLOCKS_TABLE)?;

                // collect records to migrate in this batch
                let mut batch = Vec::with_capacity(Self::BATCH_SIZE);
                // iterate over BATCH_SIZE records
                for entry in table.range(next_height..)?.take(Self::BATCH_SIZE) {
                    let (key, value) = entry?;
                    let bytes = value.value();
                    records_scanned += 1;

                    if migrator.needs_migration(&bytes[..]) {
                        let new_bytes = migrator.migrate(&bytes[..]).map_err(|e| {
                            StoreError::Migration(format!(
                                "Failed to migrate decided block at height {}: {}",
                                key.value(),
                                e
                            ))
                        })?;

                        batch.push((key.value(), new_bytes));
                    }

                    next_height = key.value().increment();
                }

                // migrate the records in this batch
                records_upgraded = batch.len();
                for (key, value) in batch {
                    table.insert(key, value)?;
                }
            }
            tx.commit()?;
            debug!("migrated batch of {} records", records_upgraded);

            stats.records_scanned += records_scanned;
            stats.records_upgraded += records_upgraded;

            // if we scanned less than the batch size, we're done
            if records_scanned < Self::BATCH_SIZE {
                break;
            }
        }

        stats.records_skipped = stats.records_scanned - stats.records_upgraded;
        stats.tables_migrated += 1;

        info!(
            scanned = stats.records_scanned,
            upgraded = stats.records_upgraded,
            "Completed decided blocks migration"
        );

        Ok(stats)
    }

    /// Migrate undecided blocks table.
    fn migrate_undecided_blocks(
        &self,
        migrator: &dyn Migrator,
    ) -> Result<MigrationStats, StoreError> {
        info!("Migrating undecided blocks table");

        let mut stats = MigrationStats::default();

        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(UNDECIDED_BLOCKS_TABLE)?;

            // Collect all records to migrate
            // This table is typically small (temporary blocks), so we can process it all at once
            let mut to_migrate = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let bytes = value.value();
                stats.records_scanned += 1;

                if migrator.needs_migration(&bytes[..]) {
                    let new_bytes = migrator.migrate(&bytes[..]).map_err(|e| {
                        StoreError::Migration(format!(
                            "Failed to migrate undecided block at {:?}: {}",
                            key.value(),
                            e
                        ))
                    })?;

                    to_migrate.push((key.value(), new_bytes));
                }
            }

            // Apply all migrations
            for (key, value) in to_migrate {
                table.insert(key, value)?;
                stats.records_upgraded += 1;
            }
        }
        tx.commit()?;

        stats.records_skipped = stats.records_scanned - stats.records_upgraded;
        stats.tables_migrated += 1;

        info!(
            scanned = stats.records_scanned,
            upgraded = stats.records_upgraded,
            "Completed undecided blocks migration"
        );

        Ok(stats)
    }

    /// Migrate pending proposal parts table.
    fn migrate_pending_parts(&self, migrator: &dyn Migrator) -> Result<MigrationStats, StoreError> {
        info!("Migrating pending proposal parts table");

        let mut stats = MigrationStats::default();

        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE)?;

            // Collect all records to migrate
            // This table is typically small (temporary data), so we can process it all at once
            let mut to_migrate = Vec::new();
            for entry in table.iter()? {
                let (key, value) = entry?;
                let bytes = value.value();
                stats.records_scanned += 1;

                if migrator.needs_migration(&bytes[..]) {
                    let new_bytes = migrator.migrate(&bytes[..]).map_err(|e| {
                        StoreError::Migration(format!(
                            "Failed to migrate pending parts at {:?}: {}",
                            key.value(),
                            e
                        ))
                    })?;

                    to_migrate.push((key.value(), new_bytes));
                }
            }

            // Apply all migrations
            for (key, value) in to_migrate {
                table.insert(key, value)?;
                stats.records_upgraded += 1;
            }
        }
        tx.commit()?;

        stats.records_skipped = stats.records_scanned - stats.records_upgraded;
        stats.tables_migrated += 1;

        info!(
            scanned = stats.records_scanned,
            upgraded = stats.records_upgraded,
            "Completed pending parts migration"
        );

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_consensus_types::{BlockHash, Height};
    use malachitebft_app_channel::app::types::core::Round;
    use tempfile::tempdir;

    /// Helper to create a test database
    fn create_test_db() -> (redb::Database, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = redb::Database::builder()
            .create(&db_path)
            .expect("Failed to create test database");
        (db, db_path)
    }

    /// Test migrator that adds a version byte if missing
    struct TestMigrator {
        from: SchemaVersion,
        to: SchemaVersion,
    }

    impl Migrator for TestMigrator {
        fn name(&self) -> &str {
            "TestMigrator"
        }

        fn source_version(&self) -> SchemaVersion {
            self.from
        }

        fn target_version(&self) -> SchemaVersion {
            self.to
        }

        fn migrate(&self, old_bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
            if old_bytes.is_empty() {
                return Err(StoreError::Decode(DecodeError::EmptyVersion));
            }

            // If already has target version, return as-is
            if old_bytes[0] == self.to.as_u8() {
                return Ok(old_bytes.to_vec());
            }

            // Otherwise, replace version byte
            let mut result = Vec::with_capacity(old_bytes.len());
            result.push(self.to.as_u8());
            result.extend_from_slice(&old_bytes[1..]);
            Ok(result)
        }

        fn needs_migration(&self, bytes: &[u8]) -> bool {
            bytes
                .first()
                .map(|&v| v == self.from.as_u8() || v == SchemaVersion::V0.as_u8())
                .unwrap_or(true)
        }
    }

    #[test]
    fn test_metadata_table_operations() {
        let (db, _path) = create_test_db();

        // Create metadata table
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(METADATA_TABLE).unwrap();
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Initially no version
        assert_eq!(coordinator.current_schema_version().unwrap(), None);

        // Set version
        coordinator
            .set_schema_version(SchemaVersion::new(1))
            .unwrap();
        assert_eq!(
            coordinator.current_schema_version().unwrap(),
            Some(SchemaVersion::new(1))
        );

        // Update version
        coordinator
            .set_schema_version(SchemaVersion::new(2))
            .unwrap();
        assert_eq!(
            coordinator.current_schema_version().unwrap(),
            Some(SchemaVersion::new(2))
        );
    }

    #[test]
    fn test_needs_migration_new_database() {
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // New database should not need migration, it gets initialized with current version
        // needs_migration() will create the metadata table if it doesn't exist
        let needs = coordinator.needs_migration(false).unwrap();
        assert!(
            !needs,
            "New database should be initialized with current version"
        );

        // Version should be set to current
        assert_eq!(
            coordinator.current_schema_version().unwrap(),
            Some(DB_SCHEMA_VERSION)
        );
    }

    #[test]
    fn test_needs_migration_outdated() {
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // Set an old version
        let old_version = DB_SCHEMA_VERSION.previous().expect("non-zero version");
        if old_version > SchemaVersion::V0 {
            coordinator.set_schema_version(old_version).unwrap();

            // Now check if migration is needed
            let needs = coordinator.needs_migration(true).unwrap();
            assert!(needs, "Should need migration when version is outdated");
        }
    }

    #[test]
    fn test_needs_migration_newer_version() {
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // Set a newer version
        coordinator
            .set_schema_version(DB_SCHEMA_VERSION.next())
            .unwrap();

        // Should error on newer version
        let result = coordinator.needs_migration(true);
        assert!(
            result.is_err(),
            "Should error when database version is newer than supported"
        );
    }

    #[test]
    fn test_migration_stats() {
        let mut stats1 = MigrationStats {
            tables_migrated: 1,
            records_scanned: 100,
            records_upgraded: 50,
            records_skipped: 50,
            duration: Duration::from_secs(1),
        };

        let stats2 = MigrationStats {
            tables_migrated: 2,
            records_scanned: 200,
            records_upgraded: 100,
            records_skipped: 100,
            duration: Duration::from_secs(2),
        };

        stats1.merge(stats2);

        assert_eq!(stats1.tables_migrated, 3);
        assert_eq!(stats1.records_scanned, 300);
        assert_eq!(stats1.records_upgraded, 150);
        assert_eq!(stats1.records_skipped, 150);
    }

    #[test]
    fn test_migrator_needs_migration() {
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };

        // No version byte -> needs migration
        assert!(migrator.needs_migration(&[]));

        // Has from version -> needs migration
        assert!(migrator.needs_migration(&[0, 1, 2, 3]));

        // Has target version -> doesn't need migration
        assert!(!migrator.needs_migration(&[1, 1, 2, 3]));

        // Has different version -> doesn't need migration (not our job)
        assert!(!migrator.needs_migration(&[2, 1, 2, 3]));
    }

    #[test]
    fn test_migrator_migrate() {
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };

        // Empty bytes -> error
        let result = migrator.migrate(&[]);
        assert!(result.is_err());

        // Migrate from version 0
        let data = vec![0, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result, vec![1, 10, 20, 30]);

        // Already at target version -> return as-is
        let data = vec![1, 10, 20, 30];
        let result = migrator.migrate(&data).unwrap();
        assert_eq!(result, vec![1, 10, 20, 30]);
    }

    #[test]
    fn test_build_migration_chain() {
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // Chain from 0 to current version
        let chain = coordinator
            .build_migration_chain(SchemaVersion::V0, DB_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(chain.len(), DB_SCHEMA_VERSION.as_u8() as usize);

        // Chain from current to current (no migrations)
        let chain = coordinator
            .build_migration_chain(DB_SCHEMA_VERSION, DB_SCHEMA_VERSION)
            .unwrap();
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn test_certificate_table_migration() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(CERTIFICATES_TABLE).unwrap();
        tx.commit().unwrap();

        // Insert some test data with old version (0)
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(CERTIFICATES_TABLE).unwrap();
            table.insert(Height::new(1), vec![0, 1, 2, 3, 4]).unwrap();
            table.insert(Height::new(2), vec![0, 5, 6, 7, 8]).unwrap();
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_certificates(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 2);
        assert_eq!(stats.records_upgraded, 2);
        assert_eq!(stats.records_skipped, 0);
        assert_eq!(stats.tables_migrated, 1);

        let db = coordinator.into_db();

        // Verify data was migrated
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(CERTIFICATES_TABLE).unwrap();
        let value = table.get(Height::new(1)).unwrap().unwrap();
        assert_eq!(value.value(), vec![1, 1, 2, 3, 4]);
    }

    #[test]
    fn test_no_migration_when_already_upgraded() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
        tx.commit().unwrap();

        // Insert data already at target version
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
            table.insert(Height::new(1), vec![1, 1, 2, 3, 4]).unwrap();
            table.insert(Height::new(2), vec![1, 5, 6, 7, 8]).unwrap();
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_decided_blocks(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 2);
        assert_eq!(stats.records_upgraded, 0);
        assert_eq!(stats.records_skipped, 2);
    }

    #[test]
    fn test_mixed_versions_in_table() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
        tx.commit().unwrap();

        // Insert mixed version data
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
            table.insert(Height::new(1), vec![0, 1, 2, 3]).unwrap(); // old version
            table.insert(Height::new(2), vec![1, 5, 6, 7]).unwrap(); // new version
            table.insert(Height::new(3), vec![0, 8, 9, 10]).unwrap(); // old version
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_decided_blocks(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 3);
        assert_eq!(stats.records_upgraded, 2);
        assert_eq!(stats.records_skipped, 1);

        let db = coordinator.into_db();

        // Verify all data is now at version 1
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();

        let value1 = table.get(Height::new(1)).unwrap().unwrap();
        assert_eq!(value1.value()[0], 1);

        let value2 = table.get(Height::new(2)).unwrap().unwrap();
        assert_eq!(value2.value()[0], 1);

        let value3 = table.get(Height::new(3)).unwrap().unwrap();
        assert_eq!(value3.value()[0], 1);
    }

    #[test]
    fn test_full_migration_flow() {
        let (db, _path) = create_test_db();

        // Create metadata table
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(METADATA_TABLE).unwrap();
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Check if migration is needed first (this initializes the schema version for a new database)
        let needs = coordinator.needs_migration(false).unwrap();
        assert!(!needs, "New database should not need migration");

        // Now migration should complete successfully with no data to migrate
        let result = coordinator.migrate();

        // Since we start at current version, no migration should be needed
        assert!(
            result.is_ok(),
            "Migration should succeed: {:?}",
            result.err()
        );
        let stats = result.unwrap();
        assert_eq!(
            stats.tables_migrated, 0,
            "No tables should be migrated when already at current version"
        );
    }

    #[test]
    fn test_needs_migration_without_metadata_table() {
        // Create old database without metadata table
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // Should handle missing metadata table gracefully
        coordinator
            .ensure_metadata_table_exists()
            .expect("Should create metadata table");

        // Simulate old database that exists
        let needs = coordinator
            .needs_migration(true)
            .expect("Should check migration status");
        assert!(needs, "Old database with no version should need migration");

        // Verify it was set to v0
        let version = coordinator
            .current_schema_version()
            .expect("Should read version");
        assert_eq!(
            version,
            Some(SchemaVersion::V0),
            "Should be set to v0 after check"
        );
    }

    #[test]
    fn test_metadata_table_exists_idempotent() {
        let (db, _path) = create_test_db();
        let coordinator = MigrationCoordinator::new(db);

        // Call multiple times - should be idempotent
        coordinator
            .ensure_metadata_table_exists()
            .expect("First call should succeed");
        coordinator
            .ensure_metadata_table_exists()
            .expect("Second call should succeed");
        coordinator
            .ensure_metadata_table_exists()
            .expect("Third call should succeed");

        // Verify table is still accessible
        assert!(
            coordinator.current_schema_version().is_ok(),
            "Table should be accessible after multiple ensure calls"
        );
    }

    #[test]
    fn test_certificate_migration_batch_size_plus_one() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(CERTIFICATES_TABLE).unwrap();
        tx.commit().unwrap();

        // Insert BATCH_SIZE + 1 records with old version (0)
        // This tests that batch processing correctly handles the boundary
        let num_records = MigrationCoordinator::BATCH_SIZE + 1;
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(CERTIFICATES_TABLE).unwrap();
            for i in 1..=num_records {
                // Store height i with data [0, i_low, i_high, ...] where i is encoded as u16
                let mut data = vec![0]; // version 0
                data.push((i & 0xFF) as u8); // low byte of i
                data.push(((i >> 8) & 0xFF) as u8); // high byte of i
                table.insert(Height::new(i as u64), data).unwrap();
            }
        }
        tx.commit().unwrap();

        // Verify we have BATCH_SIZE + 1 records before migration
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(CERTIFICATES_TABLE).unwrap();
        let count = table.iter().unwrap().count();
        assert_eq!(
            count, num_records,
            "Should have BATCH_SIZE+1 records before migration"
        );
        drop(table);
        drop(tx);

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_certificates(&migrator).unwrap();

        // Verify all records were scanned and upgraded
        assert_eq!(
            stats.records_scanned, num_records,
            "Should scan all BATCH_SIZE+1 records"
        );
        assert_eq!(
            stats.records_upgraded, num_records,
            "Should upgrade all BATCH_SIZE+1 records"
        );
        assert_eq!(stats.records_skipped, 0, "Should not skip any records");
        assert_eq!(stats.tables_migrated, 1);

        let db = coordinator.into_db();

        // Verify all records were migrated correctly
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(CERTIFICATES_TABLE).unwrap();

        for i in 1..=num_records {
            let value = table.get(Height::new(i as u64)).unwrap().unwrap();
            let bytes = value.value();

            // Should be version 1 now
            assert_eq!(
                bytes[0], 1,
                "Record at height {} should be version 1 after migration",
                i
            );

            // Data should be preserved (low byte, high byte)
            assert_eq!(
                bytes[1],
                (i & 0xFF) as u8,
                "Record at height {} should have correct low byte",
                i
            );
            assert_eq!(
                bytes[2],
                ((i >> 8) & 0xFF) as u8,
                "Record at height {} should have correct high byte",
                i
            );
        }

        // Verify count is still correct
        let count = table.iter().unwrap().count();
        assert_eq!(
            count, num_records,
            "Should still have BATCH_SIZE+1 records after migration"
        );
    }

    #[test]
    fn test_undecided_blocks_migration() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
        tx.commit().unwrap();

        // Create test block hashes
        let hash1 = BlockHash::from([1u8; 32]);
        let hash2 = BlockHash::from([2u8; 32]);
        let hash3 = BlockHash::from([3u8; 32]);

        // Insert some test data with old version (0)
        // Using composite keys (height, round, block_hash) as per table definition
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
            table
                .insert((Height::new(1), Round::new(0), hash1), vec![0, 1, 2, 3, 4])
                .unwrap();
            table
                .insert((Height::new(1), Round::new(1), hash2), vec![0, 5, 6, 7, 8])
                .unwrap();
            table
                .insert(
                    (Height::new(2), Round::new(0), hash3),
                    vec![0, 9, 10, 11, 12],
                )
                .unwrap();
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_undecided_blocks(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 3);
        assert_eq!(stats.records_upgraded, 3);
        assert_eq!(stats.records_skipped, 0);
        assert_eq!(stats.tables_migrated, 1);

        let db = coordinator.into_db();

        // Verify data was migrated
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
        let value = table
            .get((Height::new(1), Round::new(0), hash1))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 1, 2, 3, 4]);

        let value = table
            .get((Height::new(1), Round::new(1), hash2))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 5, 6, 7, 8]);

        let value = table
            .get((Height::new(2), Round::new(0), hash3))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 9, 10, 11, 12]);
    }

    #[test]
    fn test_undecided_blocks_migration_empty_table() {
        let (db, _path) = create_test_db();

        // Create empty table
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration on empty table
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_undecided_blocks(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 0);
        assert_eq!(stats.records_upgraded, 0);
        assert_eq!(stats.records_skipped, 0);
        assert_eq!(stats.tables_migrated, 1);
    }

    #[test]
    fn test_undecided_blocks_migration_mixed_versions() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
        tx.commit().unwrap();

        // Create test block hashes
        let hash1 = BlockHash::from([1u8; 32]);
        let hash2 = BlockHash::from([2u8; 32]);
        let hash3 = BlockHash::from([3u8; 32]);

        // Insert mixed version data
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
            table
                .insert((Height::new(1), Round::new(0), hash1), vec![0, 1, 2, 3])
                .unwrap(); // old version
            table
                .insert((Height::new(1), Round::new(1), hash2), vec![1, 5, 6, 7])
                .unwrap(); // new version
            table
                .insert((Height::new(2), Round::new(0), hash3), vec![0, 8, 9, 10])
                .unwrap(); // old version
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_undecided_blocks(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 3);
        assert_eq!(stats.records_upgraded, 2);
        assert_eq!(stats.records_skipped, 1);

        let db = coordinator.into_db();

        // Verify all data is now at version 1
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();

        let value1 = table
            .get((Height::new(1), Round::new(0), hash1))
            .unwrap()
            .unwrap();
        assert_eq!(value1.value()[0], 1);

        let value2 = table
            .get((Height::new(1), Round::new(1), hash2))
            .unwrap()
            .unwrap();
        assert_eq!(value2.value()[0], 1);

        let value3 = table
            .get((Height::new(2), Round::new(0), hash3))
            .unwrap()
            .unwrap();
        assert_eq!(value3.value()[0], 1);
    }

    #[test]
    fn test_pending_parts_migration() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
        tx.commit().unwrap();

        // Create test block hashes
        let hash1 = BlockHash::from([1u8; 32]);
        let hash2 = BlockHash::from([2u8; 32]);
        let hash3 = BlockHash::from([3u8; 32]);
        let hash4 = BlockHash::from([4u8; 32]);

        // Insert some test data with old version (0)
        // Using composite keys (height, round, block_hash) as per table definition
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
            table
                .insert((Height::new(1), Round::new(0), hash1), vec![0, 1, 2, 3, 4])
                .unwrap();
            table
                .insert((Height::new(1), Round::new(0), hash2), vec![0, 5, 6, 7, 8])
                .unwrap();
            table
                .insert(
                    (Height::new(1), Round::new(1), hash3),
                    vec![0, 9, 10, 11, 12],
                )
                .unwrap();
            table
                .insert(
                    (Height::new(2), Round::new(0), hash4),
                    vec![0, 13, 14, 15, 16],
                )
                .unwrap();
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_pending_parts(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 4);
        assert_eq!(stats.records_upgraded, 4);
        assert_eq!(stats.records_skipped, 0);
        assert_eq!(stats.tables_migrated, 1);

        let db = coordinator.into_db();

        // Verify data was migrated
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

        let value = table
            .get((Height::new(1), Round::new(0), hash1))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 1, 2, 3, 4]);

        let value = table
            .get((Height::new(1), Round::new(0), hash2))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 5, 6, 7, 8]);

        let value = table
            .get((Height::new(1), Round::new(1), hash3))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 9, 10, 11, 12]);

        let value = table
            .get((Height::new(2), Round::new(0), hash4))
            .unwrap()
            .unwrap();
        assert_eq!(value.value(), vec![1, 13, 14, 15, 16]);
    }

    #[test]
    fn test_pending_parts_migration_empty_table() {
        let (db, _path) = create_test_db();

        // Create empty table
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration on empty table
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_pending_parts(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 0);
        assert_eq!(stats.records_upgraded, 0);
        assert_eq!(stats.records_skipped, 0);
        assert_eq!(stats.tables_migrated, 1);
    }

    #[test]
    fn test_pending_parts_migration_mixed_versions() {
        let (db, _path) = create_test_db();

        // Create tables
        let tx = db.begin_write().unwrap();
        let _ = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
        tx.commit().unwrap();

        // Create test block hashes
        let hash1 = BlockHash::from([1u8; 32]);
        let hash2 = BlockHash::from([2u8; 32]);
        let hash3 = BlockHash::from([3u8; 32]);
        let hash4 = BlockHash::from([4u8; 32]);

        // Insert mixed version data
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
            table
                .insert((Height::new(1), Round::new(0), hash1), vec![0, 1, 2, 3])
                .unwrap(); // old version
            table
                .insert((Height::new(1), Round::new(0), hash2), vec![1, 5, 6, 7])
                .unwrap(); // new version
            table
                .insert((Height::new(1), Round::new(1), hash3), vec![0, 8, 9, 10])
                .unwrap(); // old version
            table
                .insert((Height::new(2), Round::new(0), hash4), vec![1, 11, 12, 13])
                .unwrap(); // new version
        }
        tx.commit().unwrap();

        let coordinator = MigrationCoordinator::new(db);

        // Run migration
        let migrator = TestMigrator {
            from: SchemaVersion::V0,
            to: SchemaVersion::V1,
        };
        let stats = coordinator.migrate_pending_parts(&migrator).unwrap();

        assert_eq!(stats.records_scanned, 4);
        assert_eq!(stats.records_upgraded, 2);
        assert_eq!(stats.records_skipped, 2);

        let db = coordinator.into_db();

        // Verify all data is now at version 1
        let tx = db.begin_read().unwrap();
        let table = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

        let value1 = table
            .get((Height::new(1), Round::new(0), hash1))
            .unwrap()
            .unwrap();
        assert_eq!(value1.value()[0], 1);

        let value2 = table
            .get((Height::new(1), Round::new(0), hash2))
            .unwrap()
            .unwrap();
        assert_eq!(value2.value()[0], 1);

        let value3 = table
            .get((Height::new(1), Round::new(1), hash3))
            .unwrap()
            .unwrap();
        assert_eq!(value3.value()[0], 1);

        let value4 = table
            .get((Height::new(2), Round::new(0), hash4))
            .unwrap()
            .unwrap();
        assert_eq!(value4.value()[0], 1);
    }
}
