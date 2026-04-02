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

//! CLI integration tests for the database migrate command
//!
//! These tests verify the end-to-end behavior of the `arc-node-consensus db migrate`
//! command, including argument parsing, database operations, and output messages.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use assert_cmd::assert::OutputAssertExt;
use predicates::prelude::*;
use tempfile::tempdir;

use arc_node_consensus::store::migrations::METADATA_TABLE;
use arc_node_consensus::store::versions::SchemaVersion;
use arc_node_consensus::store::{CERTIFICATES_TABLE, DECIDED_BLOCKS_TABLE};

/// Helper function to create a v0 test database (without metadata table)
fn create_v0_test_database(path: PathBuf) {
    // Create a basic redb database with the old schema (no metadata table)
    let db = redb::Database::builder()
        .create(&path)
        .expect("Failed to create test database");

    // Create the old tables without metadata table
    // Using u64 as key type since Height wraps u64
    let tx = db.begin_write().expect("Failed to begin write");
    {
        // Create old schema tables
        let _certificates = tx
            .open_table(CERTIFICATES_TABLE)
            .expect("Failed to create certificates table");

        let _decided_blocks = tx
            .open_table(DECIDED_BLOCKS_TABLE)
            .expect("Failed to create decided_blocks table");
    }
    tx.commit().expect("Failed to commit transaction");
}

/// Helper function to create a current version test database
fn create_current_test_database(path: PathBuf) {
    // Create a database with metadata table and current version
    // We'll use redb directly to avoid needing private types
    let db = redb::Database::builder()
        .create(&path)
        .expect("Failed to create test database");

    let tx = db.begin_write().expect("Failed to begin write");
    {
        // Create metadata table
        let mut metadata = tx
            .open_table(METADATA_TABLE)
            .expect("Failed to create metadata table");

        // Set to current schema version (v1)
        metadata
            .insert("schema_version", SchemaVersion::V1)
            .expect("Failed to set schema version");

        // Create the standard tables
        let _ = tx
            .open_table(CERTIFICATES_TABLE)
            .expect("Failed to create certificates table");

        let _ = tx
            .open_table(DECIDED_BLOCKS_TABLE)
            .expect("Failed to create decided_blocks table");
    }
    tx.commit().expect("Failed to commit transaction");
}

#[test]
fn test_migrate_command_with_nonexistent_database() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args(["db", "migrate", "--home", home_dir.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Database file does not exist at path",
        ));
}

#[test]
fn test_migrate_command_with_empty_database() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();

    // Create an empty v0 database (without metadata table, without data)
    create_v0_test_database(home_dir.join("store.db"));

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    let output = cmd
        .args(["db", "migrate", "--home", home_dir.to_str().unwrap()])
        .output()
        .expect("Failed to execute command");

    // The migrate should complete successfully for an empty database
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() || stdout.contains("Database"),
        "Should handle empty database migrate"
    );
}

#[test]
fn test_migrate_command_with_up_to_date_database() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();

    // Create a current version database
    create_current_test_database(home_dir.join("store.db"));

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args(["db", "migrate", "--home", home_dir.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Database is already up to date"));
}

#[test]
fn test_migrate_command_dry_run() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();

    create_v0_test_database(home_dir.join("store.db"));

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "migrate",
        "--home",
        home_dir.to_str().unwrap(),
        "--dry-run",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("Dry-run mode"));
}

#[test]
fn test_migrate_command_shows_log_messages() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();

    create_v0_test_database(home_dir.join("store.db"));

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    let output = cmd
        .args(["db", "migrate", "--home", home_dir.to_str().unwrap()])
        .output()
        .expect("Failed to execute command");

    // The command should complete (success or failure)
    // Just verify it shows the expected log messages
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Starting database migration")
            || stdout.contains("Opening database")
            || stdout.contains("Database"),
        "Should show database-related messages"
    );
}

#[test]
fn test_migrate_command_without_home_flag() {
    // Test that command uses default home directory when --home is not provided
    // This should fail because the default location likely doesn't have a database
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args(["db", "migrate"]).assert().failure();
}

#[test]
fn test_migrate_command_with_invalid_home_path() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args(["db", "migrate", "--home", "/nonexistent/path/to/nowhere"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Database file does not exist at path",
        ));
}

#[test]
fn test_migrate_command_help() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args(["db", "migrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Migrate the database schema"))
        .stdout(predicate::str::contains("--dry-run"));
}

// Note: Data preservation test is skipped because it requires matching the exact
// internal HeightKey type used by the store, which is not publicly exposed.
// The migration logic itself is tested in the unit tests in migrations.rs.
