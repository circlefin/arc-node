// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use arc_consensus_types::{Height, B256};
use arc_node_consensus::store::{
    rollback_to_height, CERTIFICATES_TABLE, DECIDED_BLOCKS_TABLE, INVALID_PAYLOADS_TABLE,
    MISBEHAVIOR_EVIDENCE_TABLE, PENDING_PROPOSAL_PARTS_TABLE, PROPOSAL_MONITOR_DATA_TABLE,
    UNDECIDED_BLOCKS_TABLE,
};
use assert_cmd::assert::OutputAssertExt;
use malachitebft_app_channel::app::types::core::Round;
use predicates::prelude::*;
use redb::ReadableTable;
use tempfile::tempdir;

fn create_test_database(path: PathBuf) {
    let db = redb::Database::builder()
        .create(&path)
        .expect("Failed to create test database");

    let tx = db.begin_write().expect("Failed to begin write");
    {
        let mut certificates = tx
            .open_table(CERTIFICATES_TABLE)
            .expect("Failed to open certificates table");
        let mut decided = tx
            .open_table(DECIDED_BLOCKS_TABLE)
            .expect("Failed to open decided blocks table");
        let mut invalid = tx
            .open_table(INVALID_PAYLOADS_TABLE)
            .expect("Failed to open invalid payloads table");
        let mut misbehavior = tx
            .open_table(MISBEHAVIOR_EVIDENCE_TABLE)
            .expect("Failed to open misbehavior evidence table");
        let mut proposal_monitor = tx
            .open_table(PROPOSAL_MONITOR_DATA_TABLE)
            .expect("Failed to open proposal monitor table");
        let mut undecided = tx
            .open_table(UNDECIDED_BLOCKS_TABLE)
            .expect("Failed to open undecided blocks table");
        let mut pending = tx
            .open_table(PENDING_PROPOSAL_PARTS_TABLE)
            .expect("Failed to open pending proposal parts table");

        for h in 1u64..=3 {
            let height = Height::new(h);
            let payload = vec![h as u8];
            certificates
                .insert(height, payload.clone())
                .expect("insert certificate");
            decided
                .insert(height, payload.clone())
                .expect("insert decided block");
            invalid
                .insert(height, payload.clone())
                .expect("insert invalid payload");
            misbehavior
                .insert(height, payload.clone())
                .expect("insert misbehavior evidence");
            proposal_monitor
                .insert(height, payload.clone())
                .expect("insert proposal monitor data");

            let block_hash = B256::repeat_byte(h as u8);
            undecided
                .insert((height, Round::new(0), block_hash), payload.clone())
                .expect("insert undecided block");
            pending
                .insert((height, Round::new(0), block_hash), payload)
                .expect("insert pending proposal parts");
        }
    }
    tx.commit().expect("Failed to commit transaction");
}

#[test]
fn test_rollback_command_removes_recent_heights() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path.clone());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--num-heights",
        "1",
        "--execute",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Database rollback completed successfully",
    ));

    let db = redb::Database::builder()
        .open(&db_path)
        .expect("Failed to reopen test database");
    let tx = db.begin_read().expect("Failed to start read transaction");

    // Height 3 must be gone from all tables
    let certificates = tx.open_table(CERTIFICATES_TABLE).unwrap();
    assert_eq!(
        certificates.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(2))
    );
    assert!(certificates.get(Height::new(3)).unwrap().is_none());

    let decided = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
    assert!(decided.get(Height::new(3)).unwrap().is_none());

    let invalid = tx.open_table(INVALID_PAYLOADS_TABLE).unwrap();
    assert!(invalid.get(Height::new(3)).unwrap().is_none());

    let misbehavior = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE).unwrap();
    assert!(misbehavior.get(Height::new(3)).unwrap().is_none());

    let proposal_monitor = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE).unwrap();
    assert!(proposal_monitor.get(Height::new(3)).unwrap().is_none());

    let undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
    assert!(undecided
        .get((Height::new(3), Round::new(0), B256::repeat_byte(3)))
        .unwrap()
        .is_none());

    let pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
    assert!(pending
        .get((Height::new(3), Round::new(0), B256::repeat_byte(3)))
        .unwrap()
        .is_none());

    // Heights 1 and 2 must be intact in all tables
    for h in [1u64, 2] {
        let height = Height::new(h);
        assert!(
            certificates.get(height).unwrap().is_some(),
            "certificates missing height {h}"
        );
        assert!(
            decided.get(height).unwrap().is_some(),
            "decided_blocks missing height {h}"
        );
        assert!(
            invalid.get(height).unwrap().is_some(),
            "invalid_payloads missing height {h}"
        );
        assert!(
            misbehavior.get(height).unwrap().is_some(),
            "misbehavior_evidence missing height {h}"
        );
        assert!(
            proposal_monitor.get(height).unwrap().is_some(),
            "proposal_monitor_data missing height {h}"
        );
        assert!(
            undecided
                .get((height, Round::new(0), B256::repeat_byte(h as u8)))
                .unwrap()
                .is_some(),
            "undecided_blocks missing height {h}"
        );
        assert!(
            pending
                .get((height, Round::new(0), B256::repeat_byte(h as u8)))
                .unwrap()
                .is_some(),
            "pending_proposal_parts missing height {h}"
        );
    }
}

#[test]
fn test_rollback_default_is_dry_run() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path.clone());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--num-heights",
        "2",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("re-run with --execute"));

    // Database must be untouched
    let db = redb::Database::builder()
        .open(&db_path)
        .expect("Failed to reopen test database");
    let tx = db.begin_read().expect("Failed to start read transaction");
    let certificates = tx.open_table(CERTIFICATES_TABLE).unwrap();

    assert_eq!(
        certificates.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(3))
    );
    assert!(certificates.get(Height::new(3)).unwrap().is_some());
}

#[test]
fn test_rollback_past_genesis_errors() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path.clone());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--num-heights",
        "10",
        "--execute",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("erase genesis"));

    // Database must be untouched
    let db = redb::Database::builder()
        .open(&db_path)
        .expect("Failed to reopen test database");
    let tx = db.begin_read().expect("Failed to start read transaction");
    let certificates = tx.open_table(CERTIFICATES_TABLE).unwrap();
    assert_eq!(
        certificates.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(3))
    );
}

#[test]
fn test_rollback_removes_all_composite_key_variations() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");

    let h2 = Height::new(2);
    let h3 = Height::new(3);
    let hash_a = B256::repeat_byte(0xAA);
    let hash_b = B256::repeat_byte(0xBB);

    // Seed: height 2 and 3 each get multiple (round, hash) combinations,
    // plus a certificate so the CLI can read current_height.
    {
        let db = redb::Database::builder()
            .create(&db_path)
            .expect("create db");
        let tx = db.begin_write().expect("begin write");
        {
            let mut certs = tx.open_table(CERTIFICATES_TABLE).expect("open certs");
            certs.insert(h2, vec![2u8]).expect("insert cert h2");
            certs.insert(h3, vec![3u8]).expect("insert cert h3");

            let mut undecided = tx
                .open_table(UNDECIDED_BLOCKS_TABLE)
                .expect("open undecided");
            let mut pending = tx
                .open_table(PENDING_PROPOSAL_PARTS_TABLE)
                .expect("open pending");

            // Height 2: two rounds, two hashes
            for round in [Round::Nil, Round::new(0), Round::new(1)] {
                for hash in [hash_a, hash_b] {
                    undecided
                        .insert((h2, round, hash), vec![2u8])
                        .expect("insert undecided h2");
                    pending
                        .insert((h2, round, hash), vec![2u8])
                        .expect("insert pending h2");
                }
            }

            // Height 3: same spread
            for round in [Round::Nil, Round::new(0), Round::new(1)] {
                for hash in [hash_a, hash_b] {
                    undecided
                        .insert((h3, round, hash), vec![3u8])
                        .expect("insert undecided h3");
                    pending
                        .insert((h3, round, hash), vec![3u8])
                        .expect("insert pending h3");
                }
            }
        }
        tx.commit().expect("commit");
    }

    // Roll back 1 height (remove height 3, keep height 2)
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--num-heights",
        "1",
        "--execute",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Database rollback completed successfully",
    ));

    let db = redb::Database::builder().open(&db_path).expect("reopen db");
    let tx = db.begin_read().expect("begin read");
    let undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
    let pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

    // Every (round, hash) combination at height 3 must be gone
    for round in [Round::Nil, Round::new(0), Round::new(1)] {
        for hash in [hash_a, hash_b] {
            assert!(
                undecided.get((h3, round, hash)).unwrap().is_none(),
                "undecided entry at h3/round={round:?}/hash={hash} should be deleted"
            );
            assert!(
                pending.get((h3, round, hash)).unwrap().is_none(),
                "pending entry at h3/round={round:?}/hash={hash} should be deleted"
            );
        }
    }

    // Every (round, hash) combination at height 2 must survive
    for round in [Round::Nil, Round::new(0), Round::new(1)] {
        for hash in [hash_a, hash_b] {
            assert!(
                undecided.get((h2, round, hash)).unwrap().is_some(),
                "undecided entry at h2/round={round:?}/hash={hash} should be retained"
            );
            assert!(
                pending.get((h2, round, hash)).unwrap().is_some(),
                "pending entry at h2/round={round:?}/hash={hash} should be retained"
            );
        }
    }
}

#[test]
fn test_rollback_with_batch_size_one() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("store.db");

    let db = redb::Database::builder()
        .create(&db_path)
        .expect("create db");

    // Seed heights 1..=5 in all 7 tables
    {
        let tx = db.begin_write().expect("begin write");
        {
            let mut certs = tx.open_table(CERTIFICATES_TABLE).unwrap();
            let mut decided = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
            let mut invalid = tx.open_table(INVALID_PAYLOADS_TABLE).unwrap();
            let mut misbehavior = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE).unwrap();
            let mut proposal_monitor = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE).unwrap();
            let mut undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
            let mut pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

            for h in 1u64..=5 {
                let height = Height::new(h);
                let payload = vec![h as u8];
                certs.insert(height, payload.clone()).unwrap();
                decided.insert(height, payload.clone()).unwrap();
                invalid.insert(height, payload.clone()).unwrap();
                misbehavior.insert(height, payload.clone()).unwrap();
                proposal_monitor.insert(height, payload.clone()).unwrap();

                let hash = B256::repeat_byte(h as u8);
                undecided
                    .insert((height, Round::new(0), hash), payload.clone())
                    .unwrap();
                pending
                    .insert((height, Round::new(0), hash), payload)
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    // Roll back to height 2, batch_size=1 forces 3 separate transactions (heights 3, 4, 5)
    let report =
        rollback_to_height(&db, Height::new(2), 1, false).expect("rollback should succeed");

    assert_eq!(report.certificates, 3);
    assert_eq!(report.decided_blocks, 3);
    assert_eq!(report.invalid_payloads, 3);
    assert_eq!(report.misbehavior_evidence, 3);
    assert_eq!(report.proposal_monitor_data, 3);
    assert_eq!(report.undecided_blocks, 3);
    assert_eq!(report.pending_proposal_parts, 3);

    let tx = db.begin_read().unwrap();

    let certs = tx.open_table(CERTIFICATES_TABLE).unwrap();
    assert_eq!(
        certs.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(2))
    );
    for h in 3u64..=5 {
        assert!(certs.get(Height::new(h)).unwrap().is_none());
    }
    for h in 1u64..=2 {
        assert!(certs.get(Height::new(h)).unwrap().is_some());
    }

    let undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
    let pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();
    for h in 3u64..=5 {
        let key = (Height::new(h), Round::new(0), B256::repeat_byte(h as u8));
        assert!(undecided.get(key).unwrap().is_none());
        assert!(pending.get(key).unwrap().is_none());
    }
    for h in 1u64..=2 {
        let key = (Height::new(h), Round::new(0), B256::repeat_byte(h as u8));
        assert!(undecided.get(key).unwrap().is_some());
        assert!(pending.get(key).unwrap().is_some());
    }
}

/// Regression test: batch_size that doesn't divide evenly into the deletion range.
/// Heights 1–7, target=2, batch_size=2 → 5 heights to delete (3,4,5,6,7).
/// Batches (high-to-low): [6,7], [4,5], [3]. The last partial batch must clamp
/// at the lower bound and not delete heights 1 or 2.
#[test]
fn test_rollback_uneven_batch_respects_lower_bound() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("store.db");

    let db = redb::Database::builder()
        .create(&db_path)
        .expect("create db");

    {
        let tx = db.begin_write().expect("begin write");
        {
            let mut certs = tx.open_table(CERTIFICATES_TABLE).unwrap();
            let mut decided = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
            let mut invalid = tx.open_table(INVALID_PAYLOADS_TABLE).unwrap();
            let mut misbehavior = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE).unwrap();
            let mut proposal_monitor = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE).unwrap();
            let mut undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
            let mut pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

            for h in 1u64..=7 {
                let height = Height::new(h);
                let payload = vec![h as u8];
                certs.insert(height, payload.clone()).unwrap();
                decided.insert(height, payload.clone()).unwrap();
                invalid.insert(height, payload.clone()).unwrap();
                misbehavior.insert(height, payload.clone()).unwrap();
                proposal_monitor.insert(height, payload.clone()).unwrap();

                let hash = B256::repeat_byte(h as u8);
                undecided
                    .insert((height, Round::new(0), hash), payload.clone())
                    .unwrap();
                pending
                    .insert((height, Round::new(0), hash), payload)
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let report =
        rollback_to_height(&db, Height::new(2), 2, false).expect("rollback should succeed");

    assert_eq!(report.certificates, 5);
    assert_eq!(report.decided_blocks, 5);
    assert_eq!(report.invalid_payloads, 5);
    assert_eq!(report.misbehavior_evidence, 5);
    assert_eq!(report.proposal_monitor_data, 5);
    assert_eq!(report.undecided_blocks, 5);
    assert_eq!(report.pending_proposal_parts, 5);

    let tx = db.begin_read().unwrap();

    // Heights 3–7 must be deleted
    let certs = tx.open_table(CERTIFICATES_TABLE).unwrap();
    for h in 3u64..=7 {
        assert!(
            certs.get(Height::new(h)).unwrap().is_none(),
            "height {h} should be deleted"
        );
    }

    // Heights 1–2 must be intact across all tables
    let decided = tx.open_table(DECIDED_BLOCKS_TABLE).unwrap();
    let invalid = tx.open_table(INVALID_PAYLOADS_TABLE).unwrap();
    let misbehavior = tx.open_table(MISBEHAVIOR_EVIDENCE_TABLE).unwrap();
    let proposal_monitor = tx.open_table(PROPOSAL_MONITOR_DATA_TABLE).unwrap();
    let undecided = tx.open_table(UNDECIDED_BLOCKS_TABLE).unwrap();
    let pending = tx.open_table(PENDING_PROPOSAL_PARTS_TABLE).unwrap();

    for h in 1u64..=2 {
        let height = Height::new(h);
        assert!(
            certs.get(height).unwrap().is_some(),
            "certs missing height {h}"
        );
        assert!(
            decided.get(height).unwrap().is_some(),
            "decided missing height {h}"
        );
        assert!(
            invalid.get(height).unwrap().is_some(),
            "invalid missing height {h}"
        );
        assert!(
            misbehavior.get(height).unwrap().is_some(),
            "misbehavior missing height {h}"
        );
        assert!(
            proposal_monitor.get(height).unwrap().is_some(),
            "proposal_monitor missing height {h}"
        );
        let key = (height, Round::new(0), B256::repeat_byte(h as u8));
        assert!(
            undecided.get(key).unwrap().is_some(),
            "undecided missing height {h}"
        );
        assert!(
            pending.get(key).unwrap().is_some(),
            "pending missing height {h}"
        );
    }

    assert_eq!(
        certs.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(2))
    );
}

#[test]
fn test_rollback_to_height_flag() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path.clone());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--to-height",
        "1",
        "--execute",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Database rollback completed successfully",
    ));

    let db = redb::Database::builder()
        .open(&db_path)
        .expect("Failed to reopen test database");
    let tx = db.begin_read().expect("Failed to start read transaction");
    let certificates = tx.open_table(CERTIFICATES_TABLE).unwrap();

    // Only height 1 should remain
    assert_eq!(
        certificates.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(1))
    );
    assert!(certificates.get(Height::new(2)).unwrap().is_none());
    assert!(certificates.get(Height::new(3)).unwrap().is_none());
}

#[test]
fn test_rollback_to_height_dry_run() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path.clone());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--to-height",
        "1",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("re-run with --execute"));

    // Database must be untouched
    let db = redb::Database::builder()
        .open(&db_path)
        .expect("Failed to reopen test database");
    let tx = db.begin_read().expect("Failed to start read transaction");
    let certificates = tx.open_table(CERTIFICATES_TABLE).unwrap();
    assert_eq!(
        certificates.last().unwrap().map(|(k, _)| k.value()),
        Some(Height::new(3))
    );
}

#[test]
fn test_rollback_conflicting_flags_errors() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--num-heights",
        "1",
        "--to-height",
        "2",
        "--execute",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_rollback_neither_flag_errors() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--execute",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "Specify exactly one of --num-heights",
    ));
}

#[test]
fn test_rollback_to_height_above_current_errors() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--to-height",
        "100",
        "--execute",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("nothing to roll back"));
}

#[test]
fn test_rollback_to_height_zero_errors() {
    let dir = tempdir().unwrap();
    let home_dir = dir.path();
    fs::create_dir_all(home_dir).unwrap();
    let db_path = home_dir.join("store.db");
    create_test_database(db_path);

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("arc-node-consensus"));
    cmd.args([
        "db",
        "rollback",
        "--home",
        home_dir.to_str().unwrap(),
        "--to-height",
        "0",
        "--execute",
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("erase genesis"));
}
