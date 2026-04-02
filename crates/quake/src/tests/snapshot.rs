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

//! Snapshot creation and restoration helpers for quake tests.
//!
//! - [`create_snapshot`]: stops a node, creates a sparse tar.lz4 archive of its
//!   EL data, then restarts the node. Returns the archive path.
//! - [`restore_from_snapshot`]: stops a target node, replaces its EL data and
//!   CL store.db from the snapshot, then restarts it.

use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::WrapErr;
use tracing::info;

use crate::infra::docker;
use crate::infra::local::COMPOSE_FILENAME;
use crate::node::EXECUTION_SUFFIX;
use crate::testnet::Testnet;

const SNAPSHOT_ARCHIVE_NAME: &str = "snapshot.tar.lz4";
const CONTAINER_DATADIR: &str = "/data/reth/execution-data";

/// Create a tar.lz4 snapshot of a node's EL data.
///
/// 1. Stops the node
/// 2. Creates a sparse tar archive inside the (stopped) EL container
/// 3. Restarts the node
/// 4. Compresses the tar with lz4 into `dest_dir/snapshot.tar.lz4`
///
/// Returns the path to the archive file.
pub(crate) async fn create_snapshot(
    testnet: &Testnet,
    node: &str,
    dest_dir: &Path,
) -> color_eyre::eyre::Result<PathBuf> {
    let testnet_dir = &testnet.dir;
    let compose_path = testnet_dir.join(COMPOSE_FILENAME);
    let el_service = format!("{node}_{EXECUTION_SUFFIX}");

    info!("🛑 Stopping {node} for snapshot");
    testnet
        .stop(vec![node.to_string()])
        .await
        .wrap_err_with(|| format!("Failed to stop {node}"))?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let tar_name = "snapshot.tar";
    info!("📦 Creating sparse tar snapshot inside {el_service}");
    create_tar_in_container(&testnet.repo_root_dir, &compose_path, &el_service, tar_name)
        .wrap_err("Failed to create tar snapshot in container")?;

    info!("🔄 Restarting {node}");
    testnet
        .start(vec![node.to_string()])
        .await
        .wrap_err_with(|| format!("Failed to restart {node}"))?;

    let provider_tar = testnet_dir.join(node).join("reth").join(tar_name);
    let archive_path = dest_dir.join(SNAPSHOT_ARCHIVE_NAME);

    info!("📦 Compressing {tar_name} with lz4");
    compress_file_lz4(&provider_tar, &archive_path)
        .wrap_err("Failed to lz4-compress snapshot tar")?;
    let _ = std::fs::remove_file(&provider_tar);

    info!(
        "📦 Archive created: {} ({} bytes)",
        archive_path.display(),
        std::fs::metadata(&archive_path)
            .map(|m| m.len())
            .unwrap_or(0)
    );

    Ok(archive_path)
}

/// Restore a node from a snapshot archive.
///
/// 1. Stops the target node
/// 2. Cleans its EL data (db, static_files, reth.toml)
/// 3. Replaces CL store.db from the snapshot provider (preserving config/)
/// 4. Runs `arc-node-execution download` to extract the archive
/// 5. Restarts the target node
pub(crate) async fn restore_from_snapshot(
    testnet: &Testnet,
    target_node: &str,
    snapshot_provider: &str,
    archive_path: &Path,
) -> color_eyre::eyre::Result<()> {
    let testnet_dir = &testnet.dir;
    let node_reth = testnet_dir.join(target_node).join("reth");
    let node_malachite = testnet_dir.join(target_node).join("malachite");
    let compose_path = testnet_dir.join(COMPOSE_FILENAME);

    info!("🛑 Stopping {target_node}");
    testnet
        .stop(vec![target_node.to_string()])
        .await
        .wrap_err_with(|| format!("Failed to stop {target_node}"))?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Clean EL data but keep the directory
    for subdir in ["db", "static_files"] {
        let path = node_reth.join(subdir);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .wrap_err_with(|| format!("Failed to remove {target_node}/reth/{subdir}"))?;
        }
    }

    let reth_toml = node_reth.join("reth.toml");
    if reth_toml.exists() {
        std::fs::remove_file(&reth_toml).wrap_err("Failed to remove reth.toml from snapshot")?;
    }

    // Replace CL store.db, preserving config/ (contains priv_validator_key.json)
    let wal_dir = node_malachite.join("wal");
    if wal_dir.exists() {
        std::fs::remove_dir_all(&wal_dir).wrap_err("Failed to remove malachite wal")?;
    }
    let existing_store = node_malachite.join("store.db");
    if existing_store.exists() {
        std::fs::remove_file(&existing_store).wrap_err("Failed to remove existing store.db")?;
    }
    let provider_store_db = testnet_dir
        .join(snapshot_provider)
        .join("malachite")
        .join("store.db");
    let target_store_db = node_malachite.join("store.db");
    std::fs::copy(&provider_store_db, &target_store_db)
        .wrap_err_with(|| format!("Failed to copy store.db to {target_node}"))?;
    info!(
        "📋 Copied store.db ({} bytes)",
        std::fs::metadata(&target_store_db)
            .map(|m| m.len())
            .unwrap_or(0)
    );

    // Copy the archive into the target node's reth directory so the container can access it
    let local_archive = node_reth.join(SNAPSHOT_ARCHIVE_NAME);
    if archive_path != local_archive {
        std::fs::copy(archive_path, &local_archive)
            .wrap_err("Failed to copy archive to target node")?;
    }

    // Run `arc-node-execution download` inside a one-off container
    let el_service = format!("{target_node}_{EXECUTION_SUFFIX}");
    let file_url = format!("file://{CONTAINER_DATADIR}/{SNAPSHOT_ARCHIVE_NAME}");

    info!("📥 Running download command in {el_service}: {file_url}");
    run_download_in_container(
        &testnet.repo_root_dir,
        &compose_path,
        &el_service,
        &file_url,
    )
    .wrap_err("download command failed inside container")?;

    // Clean up the archive
    if local_archive.exists() {
        let _ = std::fs::remove_file(&local_archive);
    }

    info!("🚀 Starting {target_node}");
    testnet
        .start(vec![target_node.to_string()])
        .await
        .wrap_err_with(|| format!("Failed to start {target_node}"))?;

    Ok(())
}

/// Create a tar archive inside a Docker container using GNU tar with sparse
/// file support (`-S`). This avoids archiving the full logical extent of
/// sparse MDBX files (4GB+ virtual vs a few MB actual).
fn create_tar_in_container(
    root_dir: &Path,
    compose_path: &Path,
    el_service: &str,
    tar_name: &str,
) -> color_eyre::eyre::Result<()> {
    let tar_cmd =
        format!("tar -cSf {CONTAINER_DATADIR}/{tar_name} -C {CONTAINER_DATADIR} db static_files");
    docker::compose_run(
        root_dir,
        compose_path,
        el_service,
        "/bin/sh",
        &["-c", &tar_cmd],
    )
}

/// Compress a file with lz4 framing.
fn compress_file_lz4(input: &Path, output: &Path) -> color_eyre::eyre::Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let reader = BufReader::new(std::fs::File::open(input)?);
    let writer = BufWriter::new(std::fs::File::create(output)?);
    let mut encoder = lz4::EncoderBuilder::new()
        .build(writer)
        .wrap_err("Failed to create lz4 encoder")?;
    std::io::copy(&mut { reader }, &mut encoder).wrap_err("Failed to compress with lz4")?;
    let (_writer, result) = encoder.finish();
    result.wrap_err("Failed to finalize lz4 compression")?;
    Ok(())
}

/// Run `arc-node-execution download` in a one-off Docker container, extracting
/// the snapshot at `file_url` into the container's data directory.
fn run_download_in_container(
    root_dir: &Path,
    compose_path: &Path,
    el_service: &str,
    file_url: &str,
) -> color_eyre::eyre::Result<()> {
    let datadir_arg = format!("--datadir={CONTAINER_DATADIR}");
    docker::compose_run(
        root_dir,
        compose_path,
        el_service,
        "/usr/local/bin/arc-node-execution",
        &[
            "download",
            &datadir_arg,
            "--chain=/app/assets/genesis.json",
            "-u",
            file_url,
        ],
    )
}
