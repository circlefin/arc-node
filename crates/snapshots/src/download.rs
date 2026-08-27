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

//! Snapshot download and extraction logic.
//!
//! EL and CL snapshots are separate `.tar.lz4` archives with bare paths (no prefix):
//! - EL archive: `db/`, `db/mdbx.dat`, `db/mdbx.lck`, `db/database.version`
//! - CL archive: `store.db`
//!
//! Each archive is extracted directly into its target directory without any path manipulation.

use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eyre::Result;
use lz4::Decoder;
use reqwest::{blocking::Client as BlockingClient, header::RANGE, Client, StatusCode};
use serde::Deserialize;
use tar::Archive;
use tokio::task;
use tracing::{info, warn};
use url::Url;

/// Base URL for the snapshot listing and download API.
pub const SNAPSHOT_API_BASE_URL: &str = "https://snapshots.arc.network/api";

const BYTE_UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
const MAX_DOWNLOAD_RETRIES: u32 = 10;
const RETRY_BACKOFF_SECS: u64 = 5;

/// Chain identifier for snapshot URL selection.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Chain {
    #[value(name = "arc-testnet")]
    Testnet,
    #[value(name = "arc-devnet")]
    Devnet,
    #[value(name = "arc-mainnet")]
    Mainnet,
}

impl Chain {
    /// Default execution data directory (same for all chains).
    pub fn default_execution_path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".arc").join("execution"))
    }

    /// Default consensus home directory (same for all chains).
    pub fn default_consensus_path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".arc").join("consensus"))
    }

    /// The chain name to pass to `arc-node-execution download --chain`, which
    /// wants the `arc-` prefixed form.
    ///
    /// That binary decides which names it accepts, not this crate — the list is
    /// `ArcChainSpecParser::SUPPORTED_CHAINS` in `arc-execution-config`. A rename
    /// there would only show up when a manifest restore hands the name over, and by
    /// then the datadir has been deleted.
    pub fn arc_chain_arg(&self) -> &'static str {
        match self {
            Self::Testnet => "arc-testnet",
            Self::Devnet => "arc-devnet",
            Self::Mainnet => "arc-mainnet",
        }
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Testnet => write!(f, "testnet"),
            Self::Devnet => write!(f, "devnet"),
            Self::Mainnet => write!(f, "mainnet"),
        }
    }
}

/// One storage v2 publication record read by both automatic resolvers.
///
/// Unlike a v1 entry, it covers both layers at one block under one prefix, so
/// callers do not cross-reference separate `layer` and top-level `key` fields by
/// block number.
#[derive(Debug, Deserialize)]
struct V2SnapshotEntry {
    network: String,
    #[serde(rename = "blockNumber")]
    block_number: u64,
    execution: V2Execution,
    /// The consensus half may still be uploading when the entry appears.
    ///
    /// `Option` without `serde(default)` accepts both an absent field and an
    /// explicit null. The selector can then use an older complete entry instead
    /// of failing to parse the whole listing.
    consensus: Option<V2Consensus>,
}

/// The execution half of a storage v2 publication read by the resolver.
///
/// The manifest value is an object key routed through `{base}/download/{key}`,
/// not a presigned URL. The resulting URL must remain query-free because reth
/// pops its last path segment and `Url::as_str` keeps the query when serializing
/// each component URL. A retained query would send every component request to a
/// 404. `components` is deliberately unmodeled because this resolver neither
/// selects components nor checks their disk requirements.
#[derive(Debug, Deserialize)]
struct V2Execution {
    #[serde(rename = "manifestKey")]
    manifest_key: String,
}

/// The consensus half of a storage v2 publication used by the archive restore.
///
/// Storage v2 still publishes `consensus.tar.lz4`, so the lz4 and tar extraction
/// path remains required. Every such key has the same last path segment, which
/// is why [`resumable_download`] binds partial bytes to their full URL.
#[derive(Debug, Deserialize)]
struct V2Consensus {
    key: String,
}

/// A storage v2 record already proven to contain both layers.
///
/// Callers receive consensus as a required value and do not repeat the upload
/// completeness check performed by [`select_latest_v2_snapshot`].
#[derive(Debug)]
struct SelectedV2Snapshot {
    block_number: u64,
    execution: V2Execution,
    consensus: V2Consensus,
}

/// The storage v2 part of the listing response read by automatic resolution.
///
/// The v1 `snapshots` array is deliberately undeclared. Serde accepts and drops
/// undeclared fields, which is intentional for v1 but caused the original bug
/// when `v2Snapshots` was undeclared. This field has no `serde(default)` because
/// deployments with storage v2 disabled omit it, and the resulting error must
/// name the field instead of claiming the publisher shipped no entries.
#[derive(Debug, Deserialize)]
struct SnapshotListResponse {
    #[serde(rename = "v2Snapshots")]
    v2_snapshots: Vec<V2SnapshotEntry>,
}

/// The execution-layer artifact to restore, and the download style it implies.
#[derive(Debug, PartialEq)]
pub enum ExecutionSnapshotSource {
    /// A reth manifest (`manifest.json`), downloaded by handing off to
    /// `arc-node-execution download`.
    Manifest(String),
    /// A single `.tar.lz4` archive, restored by arc-snapshots itself.
    Archive(String),
}

impl ExecutionSnapshotSource {
    /// Classifies a URL as a manifest or a single archive.
    pub fn from_url(url: String) -> Self {
        if is_manifest_url(&url) {
            Self::Manifest(url)
        } else {
            Self::Archive(url)
        }
    }
}

/// Everything in `url` before the query string or fragment.
fn url_path(url: &str) -> &str {
    url.split_once(['?', '#']).map_or(url, |(path, _)| path)
}

/// Query parameters a signer regenerates on every resolution, by name prefix.
const SIGNATURE_PARAM_PREFIXES: [&str; 2] = ["x-amz-", "x-goog-"];

/// Query parameters a pre-SigV4 signer regenerates, by exact name.
const SIGNATURE_PARAM_NAMES: [&str; 3] = ["signature", "expires", "awsaccesskeyid"];

/// Whether `param` is `name=value` for something a signer rewrites each time.
fn is_signature_param(param: &str) -> bool {
    let name = param
        .split_once('=')
        .map_or(param, |(name, _)| name)
        .to_ascii_lowercase();
    SIGNATURE_PARAM_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || SIGNATURE_PARAM_NAMES.contains(&name.as_str())
}

/// Turns a URL into a name for the snapshot it points at.
///
/// The name is written to `.snapshot-url` after a restore. A later run builds the
/// name of the snapshot it is about to download and compares the two to see
/// whether the work is already done. So the same snapshot must always produce the
/// same name, and different snapshots must never produce the same one.
///
/// The signature is removed, because a pre-signed URL gets a fresh one every time
/// it is handed out and the same snapshot would otherwise look new on every run.
/// The rest of the query string is kept: something like `?network=arc-devnet` says
/// which snapshot this is, and removing it would give devnet and testnet the same
/// name, so restoring one would look like it had already restored the other.
///
/// Parameters are sorted, so listing them in a different order still gives the
/// same name.
pub fn url_identity(url: &str) -> String {
    let path = url_path(url);
    let Some((_, query)) = url.split_once('?') else {
        return path.to_string();
    };
    let mut kept: Vec<&str> = query
        .split_once('#')
        .map_or(query, |(query, _)| query)
        .split('&')
        .filter(|param| !param.is_empty() && !is_signature_param(param))
        .collect();

    if kept.is_empty() {
        return path.to_string();
    }
    kept.sort_unstable();
    format!("{path}?{}", kept.join("&"))
}

/// Whether a URL points at a reth snapshot manifest rather than a single
/// archive.
///
/// Reads the path, not the [`url_identity`]: the identity may carry query
/// parameters, and the last path segment is what names the artifact.
fn is_manifest_url(url: &str) -> bool {
    url_path(url).rsplit('/').next() == Some("manifest.json")
}

/// Fetches the consensus snapshot URL from the latest complete storage v2 entry.
///
/// Using the paired publication record keeps standalone consensus resolution on
/// the same block that automatic execution resolution would select.
pub async fn fetch_latest_consensus_url(chain: Chain) -> Result<String> {
    fetch_latest_consensus_url_from(chain, SNAPSHOT_API_BASE_URL).await
}

/// Resolves the consensus download URL from the latest complete storage v2
/// entry at `base_url`. Split from
/// [`fetch_latest_consensus_url`] so tests can inject a mock server URL.
async fn fetch_latest_consensus_url_from(chain: Chain, base_url: &str) -> Result<String> {
    let selected = select_latest_v2_snapshot(chain, base_url).await?;
    Ok(format!("{}/download/{}", base_url, selected.consensus.key))
}

/// Resolve the execution and consensus snapshot sources for the given chain.
///
/// Storage v2 publishes both layers as one entry, so selecting one record keeps
/// their block heights aligned without cross-referencing separate artifacts.
pub async fn resolve_snapshot_sources(chain: Chain) -> Result<(ExecutionSnapshotSource, String)> {
    resolve_snapshot_sources_from(chain, SNAPSHOT_API_BASE_URL).await
}

/// Resolves snapshot sources from the API at `base_url`. Split from
/// [`resolve_snapshot_sources`] so tests can inject a mock server URL.
async fn resolve_snapshot_sources_from(
    chain: Chain,
    base_url: &str,
) -> Result<(ExecutionSnapshotSource, String)> {
    let selected = select_latest_v2_snapshot(chain, base_url).await?;

    info!(
        block = selected.block_number,
        "Selected storage v2 snapshot"
    );

    let execution = ExecutionSnapshotSource::Manifest(format!(
        "{}/download/{}",
        base_url, selected.execution.manifest_key
    ));
    let consensus_url = format!("{}/download/{}", base_url, selected.consensus.key);
    Ok((execution, consensus_url))
}

/// Returns the newest complete storage v2 entry for both automatic resolvers.
///
/// Both callers require consensus, so an incomplete upload must not hide an
/// older usable entry. Selecting completeness before block height preserves
/// availability while keeping both layers on one publication record.
async fn select_latest_v2_snapshot(chain: Chain, base_url: &str) -> Result<SelectedV2Snapshot> {
    fetch_v2_snapshot_entries(chain, base_url)
        .await?
        .into_iter()
        .filter_map(|entry| {
            entry.consensus.map(|consensus| SelectedV2Snapshot {
                block_number: entry.block_number,
                execution: entry.execution,
                consensus,
            })
        })
        .max_by_key(|entry| entry.block_number)
        .ok_or_else(|| {
            eyre::eyre!(
                "no complete storage v2 snapshot found for {chain}; \
                 this deployment may not publish storage v2"
            )
        })
}

/// Fetches and network-filters the storage v2 entries listed for `chain`.
///
/// The server honors the case-sensitive query and [`Chain`] renders its bare
/// network name. The client filter remains as defense in depth so a server
/// regression cannot silently select another network's snapshot.
async fn fetch_v2_snapshot_entries(chain: Chain, base_url: &str) -> Result<Vec<V2SnapshotEntry>> {
    let listing_url = format!("{}/snapshots?network={}", base_url, chain);

    let response = Client::new()
        .get(&listing_url)
        .send()
        .await?
        .error_for_status()?;
    let body = response.bytes().await?;
    let response: SnapshotListResponse = serde_json::from_slice(&body)?;
    let network = chain.to_string();
    Ok(response
        .v2_snapshots
        .into_iter()
        .filter(|e| e.network == network)
        .collect())
}

struct DownloadProgress {
    downloaded: u64,
    total_size: u64,
    last_displayed: Instant,
    started_at: Instant,
}

impl DownloadProgress {
    fn new(total_size: u64) -> Self {
        let now = Instant::now();
        Self {
            downloaded: 0,
            total_size,
            last_displayed: now,
            started_at: now,
        }
    }

    #[allow(clippy::arithmetic_side_effects)] // f64 division and index bounded by BYTE_UNITS.len()
    fn format_size(size: u64) -> String {
        let mut size = size as f64;
        let mut unit_index = 0;
        while size >= 1024.0 && unit_index < BYTE_UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }
        format!("{:.2} {}", size, BYTE_UNITS[unit_index])
    }

    #[allow(clippy::arithmetic_side_effects)] // divisors are non-zero constants
    fn format_duration(duration: Duration) -> String {
        let secs = duration.as_secs();
        if secs < 60 {
            format!("{secs}s")
        } else if secs < 3600 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        }
    }

    #[allow(clippy::arithmetic_side_effects)] // progress display math, total_size > 0 guarded
    fn update(&mut self, chunk_size: u64) -> Result<()> {
        self.downloaded = self.downloaded.saturating_add(chunk_size);
        if self.total_size == 0 {
            return Ok(());
        }
        if self.last_displayed.elapsed() >= Duration::from_millis(100) {
            let formatted_downloaded = Self::format_size(self.downloaded);
            let formatted_total = Self::format_size(self.total_size);
            let progress = (self.downloaded as f64 / self.total_size as f64) * 100.0;
            let elapsed = self.started_at.elapsed();
            let eta = if self.downloaded > 0 {
                let remaining = self.total_size.saturating_sub(self.downloaded);
                let speed = self.downloaded as f64 / elapsed.as_secs_f64();
                if speed > 0.0 {
                    Duration::from_secs_f64(remaining as f64 / speed)
                } else {
                    Duration::ZERO
                }
            } else {
                Duration::ZERO
            };
            let eta_str = Self::format_duration(eta);
            print!(
                "\rDownloading... {progress:.2}% ({formatted_downloaded} / {formatted_total}) ETA: {eta_str}     ",
            );
            io::stdout().flush()?;
            self.last_displayed = Instant::now();
        }
        Ok(())
    }
}

struct ProgressWriter<W> {
    inner: W,
    progress: DownloadProgress,
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        let _ = self.progress.update(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn file_name_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments()?.next_back().map(|s| s.to_string()))
        .unwrap_or_else(|| "snapshot.tar.lz4".to_string())
}

fn parse_total_size(response: &reqwest::blocking::Response) -> Option<u64> {
    if response.status() == StatusCode::PARTIAL_CONTENT {
        response
            .headers()
            .get("Content-Range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split('/').next_back())
            .and_then(|v| v.parse().ok())
    } else {
        response.content_length()
    }
}

fn open_part_file(part_path: &Path, append: bool) -> Result<std::fs::File> {
    if append {
        OpenOptions::new()
            .append(true)
            .open(part_path)
            .map_err(|e| eyre::eyre!("Failed to open part file {}: {e}", part_path.display()))
    } else {
        std::fs::File::create(part_path)
            .map_err(|e| eyre::eyre!("Failed to create part file {}: {e}", part_path.display()))
    }
}

/// Ensures retry logic can append only to bytes downloaded from `url`.
///
/// A stale part is deleted before its marker changes. If the marker changed first
/// and the process stopped before deletion, a later run could append to stale bytes.
/// This ordering keeps the ownership transition safe across process termination.
fn prepare_partial_download(url: &str, part_path: &Path, marker_path: &Path) -> Result<()> {
    let identity = url_identity(url);
    if matches!(std::fs::read_to_string(marker_path), Ok(saved) if saved == identity) {
        return Ok(());
    }

    match std::fs::remove_file(part_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(eyre::eyre!(
                "Failed to remove stale part file {}: {error}",
                part_path.display()
            ))
        }
    }

    std::fs::write(marker_path, identity).map_err(|error| {
        eyre::eyre!(
            "Failed to write partial download marker {}: {error}",
            marker_path.display()
        )
    })
}

/// Performs a single download attempt, appending to `part_path` if the server honours the
/// Range request. Returns the total file size reported by the server.
fn attempt_download(client: &BlockingClient, url: &str, part_path: &Path) -> Result<u64> {
    let existing_size = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);

    let mut request = client.get(url);
    if existing_size > 0 {
        request = request.header(RANGE, format!("bytes={existing_size}-"));
    }

    let mut response = request.send().and_then(|r| r.error_for_status())?;

    let is_partial = response.status() == StatusCode::PARTIAL_CONTENT;
    let total = parse_total_size(&response).ok_or_else(|| {
        eyre::eyre!("Server did not provide Content-Length or Content-Range header")
    })?;

    let file = open_part_file(part_path, is_partial && existing_size > 0)?;
    let mut progress = DownloadProgress::new(total);
    progress.downloaded = if is_partial { existing_size } else { 0 };
    let mut writer = ProgressWriter {
        inner: BufWriter::new(file),
        progress,
    };

    let result = io::copy(&mut response, &mut writer).and_then(|_| writer.inner.flush());
    println!();
    result?;

    Ok(total)
}

/// Downloads a file with resume support using HTTP Range requests.
/// Returns the path to the downloaded file and its total size.
fn resumable_download(url: &str, target_dir: &Path) -> Result<(PathBuf, u64)> {
    std::fs::create_dir_all(target_dir)?;

    let file_name = file_name_from_url(url);
    let final_path = target_dir.join(&file_name);
    let part_path = target_dir.join(format!("{file_name}.part"));
    let marker_path = target_dir.join(format!("{file_name}.part.url"));

    prepare_partial_download(url, &part_path, &marker_path)?;

    let client = BlockingClient::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()?;

    let mut last_error: Option<eyre::Error> = None;

    for attempt in 1..=MAX_DOWNLOAD_RETRIES {
        let existing_size = std::fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if attempt > 1 {
            info!("Retry attempt {attempt}/{MAX_DOWNLOAD_RETRIES} - resuming from {existing_size} bytes");
        } else if existing_size > 0 {
            info!("Resuming download from {existing_size} bytes");
        }

        match attempt_download(&client, url, &part_path) {
            Ok(total) => {
                std::fs::rename(&part_path, &final_path)?;
                if let Err(error) = std::fs::remove_file(&marker_path) {
                    warn!(
                        marker = %marker_path.display(),
                        %error,
                        "Failed to remove partial download marker after promotion"
                    );
                }
                info!("Download complete: {}", final_path.display());
                return Ok((final_path, total));
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < MAX_DOWNLOAD_RETRIES {
                    info!("Download failed, retrying in {RETRY_BACKOFF_SECS} seconds...");
                    std::thread::sleep(Duration::from_secs(RETRY_BACKOFF_SECS));
                }
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| eyre::eyre!("Download failed after {MAX_DOWNLOAD_RETRIES} attempts")))
}

/// Extracts all entries from a `.tar.lz4` archive directly into `dest_dir`.
/// Entry paths are written verbatim — no prefix stripping.
/// Aborts with an error on path traversal (absolute paths or `..` components).
fn extract_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dest_dir)?;

    let file = std::fs::File::open(archive_path)?;
    let decoder = Decoder::new(file)?;
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();

        if entry_path.as_os_str().is_empty() {
            continue;
        }

        // Reject symlinks: a symlink entry pointing outside dest_dir combined with a
        // subsequent regular-file entry through it bypasses the path checks below
        // (zip-slip via symlink).
        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
            return Err(eyre::eyre!(
                "Symlink entry rejected in archive (potential path traversal): {}",
                entry_path.display()
            ));
        }

        // Guard against path traversal: abort on ".." components or absolute paths.
        // An archive containing such entries is a strong indicator of tampering.
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(eyre::eyre!(
                "Path traversal detected in archive entry: {}",
                entry_path.display()
            ));
        }

        let dest_path = dest_dir.join(&entry_path);

        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        entry.unpack(&dest_path)?;
    }

    info!("Extraction complete");
    Ok(())
}

/// Downloads the archive at `url` into `tmp_dir` and returns where it landed.
///
/// Creates `tmp_dir` itself, and when the download fails it leaves behind what it
/// managed to fetch as a `.part` file. A later request for the same snapshot
/// continues from there instead of transferring tens of gigabytes again.
///
/// That is why callers delete `tmp_dir` only once this has returned successfully. A
/// download failure is supposed to leave it alone; a failure while unpacking is
/// not, because a broken archive is not worth resuming.
fn download_archive(url: &str, tmp_dir: &Path) -> Result<PathBuf> {
    info!(url, "Downloading snapshot");
    let (archive_path, _total_size) = resumable_download(url, tmp_dir)?;
    Ok(archive_path)
}

fn extract_downloaded_archive(archive_path: &Path, dest_dir: &Path) -> Result<()> {
    info!("Extracting snapshot");
    extract_archive(archive_path, dest_dir)
}

/// Whether `dir` already holds execution data.
///
/// `db/mdbx.dat` is the state database, written by both restore styles: the
/// lz4 archive path extracts it from the EL archive, and a manifest restore gets it
/// from the `State` component.
pub fn execution_snapshot_exists(dir: &Path) -> bool {
    dir.join("db/mdbx.dat").exists()
}

pub fn consensus_snapshot_exists(dir: &Path) -> bool {
    dir.join("store.db").exists()
}

const SNAPSHOT_VERSION_FILE: &str = ".snapshot-url";
const EXECUTION_STAGING_DIR: &str = "execution";
const CONSENSUS_STAGING_DIR: &str = "consensus";

/// Records which snapshot `dir` now holds.
///
/// Written only once a restore has finished, so the marker's presence means the
/// directory holds that snapshot, complete. [`should_download`] relies on that:
/// anything else with data in it is not this tool's to replace.
///
/// Stores [`url_identity`] rather than `url`, so the marker survives a re-signed
/// pre-signed URL and no signature is left on disk. [`should_download`]
/// normalizes the same way, so the two always compare like with like.
pub fn write_snapshot_version(dir: &Path, url: &str) -> Result<()> {
    std::fs::write(dir.join(SNAPSHOT_VERSION_FILE), url_identity(url))?;
    Ok(())
}

/// Drops the marker in `dir` before a restore invalidates what it describes.
///
/// A marker claims the directory holds a complete snapshot. A restore about to
/// overwrite that snapshot has to withdraw the claim first, or a failure partway
/// leaves the old marker beside the wreckage — and when the snapshot being
/// restored is the one the marker already names, as on a `--force` retry, the
/// next run reads it as up to date.
///
/// Only needed where a restore writes into a directory it does not remove. The
/// execution layer removes its datadir, which takes the marker along.
fn invalidate_snapshot_version(dir: &Path) -> Result<()> {
    match std::fs::remove_file(dir.join(SNAPSHOT_VERSION_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(eyre::eyre!(
            "failed to clear the snapshot marker in {}: {e}",
            dir.display()
        )),
    }
}

/// Whether the layer in `dir` needs restoring from `url`, or the run must stop.
///
/// The marker decides, not the directory contents:
///
/// - no data, or `force` — restore.
/// - marker names `url` — nothing to do.
/// - marker names another snapshot — restore. What is there is a snapshot this
///   tool wrote, so replacing it costs only the download.
/// - data with no marker — an error. It may be a node that synced from genesis
///   or a validator that has been signing since `arc-node-consensus init`, and
///   the tool cannot tell that from a restore that died before writing its
///   marker. Deleting either without being asked is worse than stopping, and
///   skipping would report success over what may be half a snapshot. `--force`
///   is how an operator says which it is.
///
/// `url` is compared as [`url_identity`], matching what
/// [`write_snapshot_version`] stored.
pub fn should_download(
    layer: &str,
    dir: &Path,
    url: &str,
    exists: bool,
    force: bool,
) -> Result<bool> {
    if force || !exists {
        return Ok(true);
    }
    match std::fs::read_to_string(dir.join(SNAPSHOT_VERSION_FILE)) {
        Ok(saved) if saved.trim() == url_identity(url) => {
            info!(dir = %dir.display(), "{layer} data already exists and is up to date, skipping download");
            Ok(false)
        }
        Ok(_) => {
            info!(dir = %dir.display(), "Newer {layer} snapshot available, re-downloading");
            Ok(true)
        }
        Err(_) => eyre::bail!(
            "{} holds data in {} that no snapshot restore recorded; pass --force to replace it",
            layer,
            dir.display()
        ),
    }
}

/// Groups the inputs that must move together for a clean pair restore.
///
/// Force restore treats EL and CL as one snapshot pair: both archives must be on
/// disk before either target is touched.
struct SnapshotPair<'a> {
    el_url: &'a str,
    cl_url: &'a str,
    execution_dir: &'a Path,
    consensus_dir: &'a Path,
    tmp_dir: &'a Path,
}

/// Removes an existing restore target and treats a missing directory as clean.
///
/// Forced restore recreates targets from the downloaded archives, so stale
/// files must not survive. Missing directories are acceptable because a fresh
/// restore may be starting from an empty data path.
pub(crate) fn remove_restore_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(eyre::eyre!(
            "failed to remove snapshot target {}: {e}",
            dir.display()
        )),
    }
}

/// Restores EL and CL as a fresh snapshot pair.
///
/// Downloads both archives into staging before touching either target, so a
/// failed second download cannot purge data the first one replaced. Only the
/// execution directory is then removed; see [`restore_consensus`] for why the
/// consensus one is not.
fn force_download_and_extract_both(pair: SnapshotPair<'_>) -> Result<()> {
    let el_tmp_dir = pair.tmp_dir.join(EXECUTION_STAGING_DIR);
    let cl_tmp_dir = pair.tmp_dir.join(CONSENSUS_STAGING_DIR);

    let el_archive = download_archive(pair.el_url, &el_tmp_dir)?;
    let cl_archive = download_archive(pair.cl_url, &cl_tmp_dir)?;

    // Only the execution directory. The consensus directory is a malachite home
    // and holds files no snapshot restores; see `restore_consensus`.
    remove_restore_dir(pair.execution_dir)?;

    let extract_result = (|| {
        extract_downloaded_archive(&el_archive, pair.execution_dir)?;
        write_snapshot_version(pair.execution_dir, pair.el_url)?;
        extract_consensus_store(&cl_archive, pair.consensus_dir, pair.cl_url)
    })();

    if let Err(e) = extract_result {
        let _ = std::fs::remove_dir_all(pair.tmp_dir);
        return Err(e);
    }

    std::fs::remove_dir_all(pair.tmp_dir)?;
    info!("Removed snapshot staging directory");
    Ok(())
}

/// Restores the execution layer from an archive, replacing what was there.
///
/// Extraction writes the files the archive names and deletes nothing else, so
/// unpacking onto a datadir another restore left behind keeps whatever the new
/// archive does not name: `static_files/` jars covering block ranges the restored
/// database has no checkpoints for, a `rocksdb/` from an earlier archive-profile
/// manifest restore, or a `reth.toml`. The target is removed first, but only
/// after the archive is staged, so a failed download costs nothing.
///
/// Nothing in the execution directory belongs to the operator, which is what
/// makes removing it safe; the consensus directory is a malachite home and is not
/// treated this way.
fn replace_from_archive(url: &str, execution_dir: &Path, staging_dir: &Path) -> Result<()> {
    let archive_path = download_archive(url, staging_dir)?;
    remove_restore_dir(execution_dir)?;

    let result = extract_downloaded_archive(&archive_path, execution_dir)
        .and_then(|()| write_snapshot_version(execution_dir, url));

    let _ = std::fs::remove_dir_all(staging_dir);
    result
}

/// Unpacks a staged consensus archive over the store in `consensus_dir`.
///
/// The one place the consensus store is written, so the marker rule is stated
/// once. The directory is not removed — [`restore_consensus`] carries why — so its
/// marker is withdrawn before extraction starts. Left in place, a failure partway
/// through `store.db` would leave a marker claiming the store is intact.
fn extract_consensus_store(archive_path: &Path, consensus_dir: &Path, url: &str) -> Result<()> {
    invalidate_snapshot_version(consensus_dir)?;
    extract_downloaded_archive(archive_path, consensus_dir)?;
    write_snapshot_version(consensus_dir, url)
}

/// Downloads the consensus archive at `url` and unpacks it over the store.
///
/// Staging happens before anything is written, so a failed download changes
/// nothing at all.
fn replace_consensus_store(url: &str, consensus_dir: &Path, staging_dir: &Path) -> Result<()> {
    let archive_path = download_archive(url, staging_dir)?;
    let result = extract_consensus_store(&archive_path, consensus_dir, url);
    let _ = std::fs::remove_dir_all(staging_dir);
    result
}

/// Downloads and extracts both EL and CL archives.
///
/// Each layer is restored only if [`should_download`] says so. A forced restore
/// stages both archives before touching either target, so the pair moves
/// together. Either way the execution directory is removed before extraction and
/// the consensus directory is not; [`restore_consensus`] carries the reason for
/// the asymmetry.
///
/// Uses `tmp_dir/execution` and `tmp_dir/consensus` as staging areas.
pub fn download_and_extract_both(
    el_url: &str,
    cl_url: &str,
    execution_dir: &Path,
    consensus_dir: &Path,
    tmp_dir: &Path,
    force_redownload: bool,
) -> Result<()> {
    if force_redownload {
        return force_download_and_extract_both(SnapshotPair {
            el_url,
            cl_url,
            execution_dir,
            consensus_dir,
            tmp_dir,
        });
    }

    // Both decisions before either restore: one layer refusing must not leave the
    // other already replaced.
    let restore_execution = should_download(
        "Execution layer",
        execution_dir,
        el_url,
        execution_snapshot_exists(execution_dir),
        force_redownload,
    )?;
    let restore_consensus = should_download(
        "Consensus layer",
        consensus_dir,
        cl_url,
        consensus_snapshot_exists(consensus_dir),
        force_redownload,
    )?;

    if restore_execution {
        replace_from_archive(el_url, execution_dir, &tmp_dir.join(EXECUTION_STAGING_DIR))?;
    }
    if restore_consensus {
        replace_consensus_store(cl_url, consensus_dir, &tmp_dir.join(CONSENSUS_STAGING_DIR))?;
    }

    // Each restore removes its own staging subdir; remove the parent if empty.
    let _ = std::fs::remove_dir(tmp_dir);
    Ok(())
}

/// Restores the consensus layer from a single `.tar.lz4` archive.
///
/// Restores only when [`should_download`] says so. The archive is staged before
/// anything is written, so a failed download leaves the existing store alone.
///
/// The target directory is never deleted, not even under `--force`. It is the
/// malachite home, and it holds two files no snapshot puts back:
/// `config/priv_validator_key.json` and `wal/consensus.wal`. Deleting it would
/// buy nothing anyway — the archive contains only `store.db`, and extraction
/// truncates that file, so the store is fully replaced either way. What the
/// surviving directory does require is that the marker be deleted before
/// extraction rather than overwritten after it. A consensus archive that grows
/// beyond `store.db` would need this revisited.
///
/// Keeping the WAL is a safety decision. Every time malachite starts a height it
/// compares the height recorded in that file against the one it is about to run.
/// They differ in the ordinary case and the log is wiped, so a WAL left from
/// before the restore usually goes away on its own. They match only when the node
/// had already started that height and died partway through it, and then the log
/// is replayed.
///
/// Replay is the reason to keep it. The log is not a list of votes this node
/// signed; it is everything the node took in at that height, in arrival order —
/// votes and proposals from any validator, proposed values, elapsed timeouts,
/// polka certificates. Feeding that sequence back rebuilds the state the node
/// was in, so it signs the same vote as before instead of a conflicting one.
/// Delete the WAL and the node comes back to that height knowing nothing, free
/// to vote for something else.
///
/// That covers a crash, not a rewind. A snapshot that puts the node below a
/// height it has already voted at gets no help from the WAL, because malachite
/// discards a log recorded at a higher height exactly as quietly as a stale one.
/// What stops that restore is [`should_download`] refusing to touch data no
/// restore recorded until the operator passes `--force`.
pub fn restore_consensus(
    url: &str,
    consensus_dir: &Path,
    tmp_dir: &Path,
    force: bool,
) -> Result<()> {
    if !should_download(
        "Consensus layer",
        consensus_dir,
        url,
        consensus_snapshot_exists(consensus_dir),
        force,
    )? {
        return Ok(());
    }

    let result = replace_consensus_store(url, consensus_dir, &tmp_dir.join(CONSENSUS_STAGING_DIR));
    let _ = std::fs::remove_dir(tmp_dir);
    result
}

/// Async wrapper: runs the consensus restore on a blocking thread.
pub async fn stream_restore_consensus(
    url: String,
    consensus_dir: PathBuf,
    tmp_dir: PathBuf,
    force: bool,
) -> Result<()> {
    task::spawn_blocking(move || restore_consensus(&url, &consensus_dir, &tmp_dir, force)).await?
}

/// Async wrapper: runs the combined EL+CL download+extract on a single blocking thread.
pub async fn stream_and_extract_both(
    el_url: String,
    cl_url: String,
    execution_dir: PathBuf,
    consensus_dir: PathBuf,
    tmp_dir: PathBuf,
    force_redownload: bool,
) -> Result<()> {
    task::spawn_blocking(move || {
        download_and_extract_both(
            &el_url,
            &cl_url,
            &execution_dir,
            &consensus_dir,
            &tmp_dir,
            force_redownload,
        )
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Build an in-memory `.tar.lz4` archive containing the given `(path, content)` entries.
    fn build_tar_lz4(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *content)?;
        }
        let (buf, result) = builder.into_inner()?.finish();
        result?;
        Ok(buf)
    }

    /// Builds an archive that unpacks `path` and then fails.
    ///
    /// The trailing symlink entry is rejected mid-stream, so extraction leaves
    /// the first file on disk — the shape a restore killed partway has, and the
    /// only one where the target ends up holding part of a snapshot.
    fn build_tar_lz4_failing_after(path: &str, content: &[u8]) -> Result<Vec<u8>> {
        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);

        let mut good = tar::Header::new_gnu();
        good.set_size(content.len() as u64);
        good.set_mode(0o644);
        good.set_cksum();
        builder.append_data(&mut good, path, content)?;

        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        let gnu = link.as_gnu_mut().expect("gnu header");
        gnu.linkname[..b"/etc\0".len()].copy_from_slice(b"/etc\0");
        gnu.name[..b"link\0".len()].copy_from_slice(b"link\0");
        link.set_cksum();
        builder.append(&link, b"".as_ref())?;

        let (buf, result) = builder.into_inner()?.finish();
        result?;
        Ok(buf)
    }

    /// Write `data` to `<dir>/<name>` and return the path.
    fn write_file(dir: &std::path::Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    fn seed_partial_download(
        target_dir: &Path,
        url: &str,
        data: &[u8],
    ) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(target_dir)?;
        let file_name = file_name_from_url(url);
        let part_path = target_dir.join(format!("{file_name}.part"));
        let marker_path = target_dir.join(format!("{file_name}.part.url"));
        std::fs::write(&part_path, data)?;
        std::fs::write(&marker_path, url_identity(url))?;
        Ok((part_path, marker_path))
    }

    async fn run_resumable_download(url: String, target_dir: PathBuf) -> Result<(PathBuf, u64)> {
        tokio::task::spawn_blocking(move || resumable_download(&url, &target_dir)).await?
    }

    async fn mount_full_download(server: &wiremock::MockServer, request_path: &str, body: &[u8]) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("GET"))
            .and(path(request_path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.to_vec())
                    .append_header("Content-Length", body.len().to_string().as_str()),
            )
            .expect(1)
            .mount(server)
            .await;
    }

    async fn request_range(server: &wiremock::MockServer) -> Result<Option<String>> {
        let requests = server
            .received_requests()
            .await
            .ok_or_else(|| eyre::eyre!("Request recording is disabled"))?;
        if requests.len() != 1 {
            return Err(eyre::eyre!(
                "Expected one request, received {}",
                requests.len()
            ));
        }
        requests[0]
            .headers
            .get("range")
            .map(|value| value.to_str().map(str::to_string).map_err(Into::into))
            .transpose()
    }

    #[tokio::test]
    async fn resumable_download_resumes_the_same_url() -> Result<()> {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let url = format!("{}/snap/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        seed_partial_download(dir.path(), &url, b"prefix-")?;
        Mock::given(method("GET"))
            .and(path("/snap/consensus.tar.lz4"))
            .and(header("range", "bytes=7-"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(b"rest".to_vec())
                    .append_header("Content-Range", "bytes 7-10/11"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let (path, total) = run_resumable_download(url, dir.path().to_path_buf()).await?;

        assert_eq!(std::fs::read(path)?, b"prefix-rest");
        assert_eq!(total, 11);
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_resumes_with_a_refreshed_signature() -> Result<()> {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let bare_url = format!("{}/snap/consensus.tar.lz4", server.uri());
        let old_url = format!("{bare_url}?X-Amz-Signature=old");
        let new_url = format!("{bare_url}?X-Amz-Signature=new");
        let dir = tempfile::tempdir()?;
        seed_partial_download(dir.path(), &old_url, b"prefix-")?;
        Mock::given(method("GET"))
            .and(path("/snap/consensus.tar.lz4"))
            .and(header("range", "bytes=7-"))
            .respond_with(
                ResponseTemplate::new(206)
                    .set_body_bytes(b"rest".to_vec())
                    .append_header("Content-Range", "bytes 7-10/11"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let (path, _) = run_resumable_download(new_url, dir.path().to_path_buf()).await?;

        assert_eq!(std::fs::read(path)?, b"prefix-rest");
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_discards_a_different_path_with_the_same_name() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let old_url = format!("{}/block-a/consensus.tar.lz4", server.uri());
        let new_url = format!("{}/block-b/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        seed_partial_download(dir.path(), &old_url, b"block-a-prefix")?;
        mount_full_download(&server, "/block-b/consensus.tar.lz4", b"block-b-archive").await;

        let (path, _) = run_resumable_download(new_url, dir.path().to_path_buf()).await?;

        assert_eq!(request_range(&server).await?, None);
        assert_eq!(std::fs::read(path)?, b"block-b-archive");
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_discards_the_same_path_from_another_host() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let path = "/block/consensus.tar.lz4";
        let old_url = format!("http://snapshot.example{path}");
        let new_url = format!("{}{path}", server.uri());
        let dir = tempfile::tempdir()?;
        seed_partial_download(dir.path(), &old_url, b"other-host")?;
        mount_full_download(&server, path, b"current-host").await;

        let (downloaded_path, _) =
            run_resumable_download(new_url, dir.path().to_path_buf()).await?;

        assert_eq!(request_range(&server).await?, None);
        assert_eq!(std::fs::read(downloaded_path)?, b"current-host");
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_discards_a_non_signature_query_change() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let bare_url = format!("{}/consensus.tar.lz4", server.uri());
        let old_url = format!("{bare_url}?network=devnet");
        let new_url = format!("{bare_url}?network=testnet");
        let dir = tempfile::tempdir()?;
        seed_partial_download(dir.path(), &old_url, b"devnet")?;
        mount_full_download(&server, "/consensus.tar.lz4", b"testnet").await;

        let (path, _) = run_resumable_download(new_url, dir.path().to_path_buf()).await?;

        assert_eq!(request_range(&server).await?, None);
        assert_eq!(std::fs::read(path)?, b"testnet");
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_discards_a_part_without_a_marker() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let url = format!("{}/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        let (_, marker_path) = seed_partial_download(dir.path(), &url, b"unowned")?;
        std::fs::remove_file(marker_path)?;
        mount_full_download(&server, "/consensus.tar.lz4", b"fresh").await;

        let (path, _) = run_resumable_download(url, dir.path().to_path_buf()).await?;

        assert_eq!(request_range(&server).await?, None);
        assert_eq!(std::fs::read(path)?, b"fresh");
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_discards_an_unreadable_or_mismatched_marker() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let url = format!("{}/consensus.tar.lz4", server.uri());
        Mock::given(method("GET"))
            .and(path("/consensus.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"fresh".to_vec())
                    .append_header("Content-Length", "5"),
            )
            .expect(2)
            .mount(&server)
            .await;

        for marker in [b"\xff".as_slice(), b"different identity".as_slice()] {
            let dir = tempfile::tempdir()?;
            let (_, marker_path) = seed_partial_download(dir.path(), &url, b"stale")?;
            std::fs::write(marker_path, marker)?;

            let (path, _) = run_resumable_download(url.clone(), dir.path().to_path_buf()).await?;

            assert_eq!(std::fs::read(path)?, b"fresh");
        }

        let requests = server
            .received_requests()
            .await
            .ok_or_else(|| eyre::eyre!("Request recording is disabled"))?;
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.headers.get("range").is_none()));
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_stops_when_the_marker_cannot_be_written() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/consensus.tar.lz4"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let url = format!("{}/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        let part_path = dir.path().join("consensus.tar.lz4.part");
        let marker_path = dir.path().join("consensus.tar.lz4.part.url");
        std::fs::write(&part_path, b"stale")?;
        std::fs::create_dir(&marker_path)?;

        let error = run_resumable_download(url, dir.path().to_path_buf())
            .await
            .expect_err("an unwritable marker must stop the download");

        assert!(error.to_string().contains("partial download marker"));
        assert!(!part_path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_removes_the_marker_after_promotion() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let url = format!("{}/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        mount_full_download(&server, "/consensus.tar.lz4", b"complete").await;

        let (path, _) = run_resumable_download(url, dir.path().to_path_buf()).await?;

        assert_eq!(std::fs::read(path)?, b"complete");
        assert!(!dir.path().join("consensus.tar.lz4.part").exists());
        assert!(!dir.path().join("consensus.tar.lz4.part.url").exists());
        Ok(())
    }

    #[tokio::test]
    async fn resumable_download_succeeds_when_the_marker_cannot_be_removed() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let url = format!("{}/consensus.tar.lz4", server.uri());
        let dir = tempfile::tempdir()?;
        let marker_path = dir.path().join("consensus.tar.lz4.part.url");
        let responder_marker = marker_path.clone();
        Mock::given(method("GET"))
            .and(path("/consensus.tar.lz4"))
            .respond_with(move |_request: &wiremock::Request| {
                std::fs::remove_file(&responder_marker)
                    .expect("the ownership marker must exist before the request");
                std::fs::create_dir(&responder_marker)
                    .expect("the marker path must become unremovable as a file");
                ResponseTemplate::new(200)
                    .set_body_bytes(b"complete".to_vec())
                    .append_header("Content-Length", "8")
            })
            .expect(1)
            .mount(&server)
            .await;

        let (downloaded_path, total) =
            run_resumable_download(url, dir.path().to_path_buf()).await?;

        assert_eq!(std::fs::read(downloaded_path)?, b"complete");
        assert_eq!(total, 8);
        assert!(marker_path.is_dir());
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Chain
    // ---------------------------------------------------------------------------

    #[test]
    fn chain_display() {
        assert_eq!(Chain::Testnet.to_string(), "testnet");
        assert_eq!(Chain::Devnet.to_string(), "devnet");
        assert_eq!(Chain::Mainnet.to_string(), "mainnet");
    }

    #[test]
    fn arc_chain_arg_matches_the_clap_value_name() {
        // `arc_chain_arg` restates the #[value(name = ...)] strings, so nothing
        // but this keeps the two in step. Iterating value_variants() covers a
        // new chain automatically.
        use clap::ValueEnum;

        for chain in Chain::value_variants() {
            let value = chain.to_possible_value().unwrap();
            assert_eq!(chain.arc_chain_arg(), value.get_name());
            // The two renderings are deliberately different: Display is the
            // snapshot API's network name, arc_chain_arg is reth's.
            assert_ne!(chain.arc_chain_arg(), chain.to_string());
        }
    }

    #[test]
    fn execution_snapshot_source_classifies_by_last_path_segment() {
        // Left column: manifests. Right column: everything else, which restores
        // natively. `--execution-url` is user-facing, so the shapes an operator
        // can plausibly paste are all pinned here.
        let manifests = [
            "https://x.example/snap/manifest.json",
            // Pre-signed URLs carry a query string.
            "https://x.example/snap/manifest.json?X-Amz-Signature=deadbeef",
            "https://x.example/snap/manifest.json#fragment",
            // Explicit inputs may also use paths without an authority.
            "testnet/manifest.json",
            "manifest.json",
        ];
        let archives = [
            "https://x.example/snap/el.tar.lz4",
            // A different file that merely ends in the same characters.
            "https://x.example/snap/el-manifest.json",
            "https://x.example/snap/notamanifest.json",
            // A directory of that name is not the file.
            "https://x.example/manifest.json/el.tar.lz4",
        ];

        for url in manifests {
            assert_eq!(
                ExecutionSnapshotSource::from_url(url.to_string()),
                ExecutionSnapshotSource::Manifest(url.to_string()),
                "expected a manifest: {url}"
            );
        }
        for url in archives {
            assert_eq!(
                ExecutionSnapshotSource::from_url(url.to_string()),
                ExecutionSnapshotSource::Archive(url.to_string()),
                "expected an archive: {url}"
            );
        }
    }

    #[tokio::test]
    async fn resolve_snapshot_sources_prefers_manifest() {
        let (uri, result) = resolve_listing(&[v2_snapshot_entry("testnet", "archive", 200)]).await;

        let (execution, consensus) = result.unwrap();
        assert_eq!(
            execution,
            ExecutionSnapshotSource::Manifest(format!(
                "{uri}/download/testnet/storage-v2/archive/200/execution/manifest.json"
            ))
        );
        assert_eq!(
            consensus,
            format!("{uri}/download/testnet/storage-v2/archive/200/consensus.tar.lz4")
        );
        assert!(!execution_url(&execution).contains('?'));
        assert!(!consensus.contains('?'));
    }

    #[tokio::test]
    async fn resolve_snapshot_sources_selects_the_highest_complete_v2_block() {
        let (uri, result) = resolve_listing(&[
            v2_snapshot_entry("testnet", "archive", 100),
            v2_snapshot_entry("testnet", "archive", 300),
            v2_snapshot_entry("testnet", "archive", 200),
        ])
        .await;

        let (execution, consensus) = result.unwrap();
        assert_eq!(
            execution,
            ExecutionSnapshotSource::Manifest(format!(
                "{uri}/download/testnet/storage-v2/archive/300/execution/manifest.json"
            ))
        );
        assert!(consensus.contains("/archive/300/"));
    }

    #[tokio::test]
    async fn both_resolvers_select_from_the_same_v2_entry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let entries = [
            v2_snapshot_entry("testnet", "archive", 100),
            v2_snapshot_entry("testnet", "archive", 200),
        ];
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(snapshot_listing(&entries)))
            .expect(2)
            .mount(&server)
            .await;

        let (_, paired_consensus) = resolve_snapshot_sources_from(Chain::Testnet, &server.uri())
            .await
            .unwrap();
        let standalone = fetch_latest_consensus_url_from(Chain::Testnet, &server.uri())
            .await
            .unwrap();

        assert_eq!(paired_consensus, standalone);
        assert!(standalone.contains("/archive/200/"));
    }

    #[tokio::test]
    async fn a_newer_incomplete_v2_entry_does_not_hide_an_older_complete_one() {
        let mut incomplete = v2_snapshot_entry("testnet", "archive", 300);
        incomplete.as_object_mut().unwrap().remove("consensus");
        let (uri, result) =
            resolve_listing(&[v2_snapshot_entry("testnet", "archive", 200), incomplete]).await;

        let (execution, consensus) = result.unwrap();
        assert_eq!(
            execution,
            ExecutionSnapshotSource::Manifest(format!(
                "{uri}/download/testnet/storage-v2/archive/200/execution/manifest.json"
            ))
        );
        assert!(consensus.contains("/archive/200/"));
    }

    #[tokio::test]
    async fn a_null_consensus_is_treated_as_incomplete() {
        let mut incomplete = v2_snapshot_entry("testnet", "archive", 300);
        incomplete["consensus"] = serde_json::Value::Null;
        let (_uri, result) =
            resolve_listing(&[v2_snapshot_entry("testnet", "archive", 200), incomplete]).await;

        let (_, consensus) = result.unwrap();
        assert!(consensus.contains("/archive/200/"));
    }

    #[tokio::test]
    async fn resolve_snapshot_sources_ignores_another_network() {
        let (_uri, result) = resolve_listing(&[
            v2_snapshot_entry("devnet", "archive", 999),
            v2_snapshot_entry("testnet", "archive", 200),
        ])
        .await;

        let (_, consensus) = result.unwrap();
        assert!(consensus.contains("/testnet/"));
        assert!(!consensus.contains("/devnet/"));
    }

    #[tokio::test]
    async fn legacy_snapshot_contents_do_not_affect_v2_selection() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "snapshots": [
                "arbitrary",
                { "blockNumber": 999999, "key": "testnet/v1.tar.lz4" }
            ],
            "v2Snapshots": [v2_snapshot_entry("testnet", "archive", 200)],
        });
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let (_, consensus) = resolve_snapshot_sources_from(Chain::Testnet, &server.uri())
            .await
            .unwrap();

        assert!(consensus.contains("/archive/200/"));
    }

    #[tokio::test]
    async fn an_empty_v2_listing_reports_the_storage_v2_error() {
        let (_uri, result) = resolve_listing(&[]).await;

        let error = result.unwrap_err().to_string();
        assert!(error.contains("testnet"), "unexpected: {error}");
        assert!(
            error.contains("may not publish storage v2"),
            "unexpected: {error}"
        );
    }

    #[tokio::test]
    async fn an_absent_v2_listing_reports_the_serde_field_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "snapshots": [] })),
            )
            .mount(&server)
            .await;

        let error = resolve_snapshot_sources_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("missing field `v2Snapshots`"),
            "unexpected: {error}"
        );
    }

    #[tokio::test]
    async fn v2_retention_values_compete_on_block_number_alone() {
        let (_uri, result) = resolve_listing(&[
            v2_snapshot_entry("testnet", "archive", 200),
            v2_snapshot_entry("testnet", "pruned", 300),
        ])
        .await;

        let (execution, consensus) = result.unwrap();
        assert!(execution_url(&execution).contains("/pruned/300/"));
        assert!(consensus.contains("/pruned/300/"));
    }

    /// Serves `entries` as a v2 listing and resolves sources from it.
    async fn resolve_listing(
        entries: &[serde_json::Value],
    ) -> (String, Result<(ExecutionSnapshotSource, String)>) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(snapshot_listing(entries)))
            .mount(&server)
            .await;

        let uri = server.uri();
        let result = resolve_snapshot_sources_from(Chain::Testnet, &uri).await;
        (uri, result)
    }

    fn execution_url(source: &ExecutionSnapshotSource) -> &str {
        match source {
            ExecutionSnapshotSource::Manifest(url) | ExecutionSnapshotSource::Archive(url) => url,
        }
    }

    #[test]
    fn chain_default_execution_path_ends_with_arc_execution() {
        // BaseDirs resolves on any OS with a home dir; in CI HOME is always set.
        if let Some(p) = Chain::default_execution_path() {
            assert!(p.ends_with(".arc/execution"));
        }
    }

    #[test]
    fn chain_default_consensus_path_ends_with_arc_consensus() {
        if let Some(p) = Chain::default_consensus_path() {
            assert!(p.ends_with(".arc/consensus"));
        }
    }

    // ---------------------------------------------------------------------------
    // DownloadProgress helpers
    // ---------------------------------------------------------------------------

    #[test]
    fn format_size_bytes() {
        assert_eq!(DownloadProgress::format_size(0), "0.00 B");
        assert_eq!(DownloadProgress::format_size(512), "512.00 B");
    }

    #[test]
    fn format_size_kilobytes() {
        assert_eq!(DownloadProgress::format_size(1024), "1.00 KB");
        assert_eq!(DownloadProgress::format_size(2048), "2.00 KB");
    }

    #[test]
    fn format_size_megabytes() {
        assert_eq!(DownloadProgress::format_size(1024 * 1024), "1.00 MB");
    }

    #[test]
    fn format_size_gigabytes() {
        assert_eq!(DownloadProgress::format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(
            DownloadProgress::format_duration(Duration::from_secs(45)),
            "45s"
        );
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(
            DownloadProgress::format_duration(Duration::from_secs(90)),
            "1m 30s"
        );
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(
            DownloadProgress::format_duration(Duration::from_secs(3660)),
            "1h 1m"
        );
    }

    #[test]
    fn progress_update_zero_total_size_is_noop() {
        let mut p = DownloadProgress::new(0);
        // Should not divide-by-zero or panic
        assert!(p.update(100).is_ok());
    }

    // ---------------------------------------------------------------------------
    // extract_archive
    // ---------------------------------------------------------------------------

    #[test]
    fn extract_archive_bare_paths() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let data = build_tar_lz4(&[("db/mdbx.dat", b"mdbx-data"), ("store.db", b"store-data")])?;
        let archive_path = write_file(dir.path(), "test.tar.lz4", &data);
        let dest = dir.path().join("dest");

        extract_archive(&archive_path, &dest)?;

        assert!(dest.join("db/mdbx.dat").exists());
        assert!(dest.join("store.db").exists());
        Ok(())
    }

    #[test]
    fn extract_archive_creates_dest_dir_if_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let data = build_tar_lz4(&[("hello.txt", b"hi")])?;
        let archive_path = write_file(dir.path(), "a.tar.lz4", &data);
        let dest = dir.path().join("new/nested/dest");

        extract_archive(&archive_path, &dest)?;

        assert!(dest.join("hello.txt").exists());
        Ok(())
    }

    #[test]
    fn extract_archive_preserves_file_content() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let content = b"exact content check";
        let data = build_tar_lz4(&[("file.txt", content)])?;
        let archive_path = write_file(dir.path(), "a.tar.lz4", &data);
        let dest = dir.path().join("dest");

        extract_archive(&archive_path, &dest)?;

        assert_eq!(std::fs::read(dest.join("file.txt"))?, content);
        Ok(())
    }

    #[test]
    fn extract_archive_rejects_absolute_path() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("dest");

        // Craft absolute path directly in the GNU header name field to bypass tar crate checks.
        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        let name_bytes = b"/etc/crontab\0";
        header.as_gnu_mut().unwrap().name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder.append(&header, b"evil".as_ref())?;
        let (buf, result) = builder.into_inner()?.finish();
        result?;
        let archive_path = write_file(dir.path(), "evil.tar.lz4", &buf);

        let err = extract_archive(&archive_path, &dest).unwrap_err();
        assert!(err.to_string().contains("Path traversal"));
        Ok(())
    }

    #[test]
    fn extract_archive_rejects_symlink() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("dest");

        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        // link name (target of symlink)
        header.as_gnu_mut().unwrap().linkname[..b"/etc\0".len()].copy_from_slice(b"/etc\0");
        let name_bytes = b"db/link\0";
        header.as_gnu_mut().unwrap().name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder.append(&header, b"".as_ref())?;
        let (buf, result) = builder.into_inner()?.finish();
        result?;
        let archive_path = write_file(dir.path(), "symlink.tar.lz4", &buf);

        let err = extract_archive(&archive_path, &dest).unwrap_err();
        assert!(err.to_string().contains("Symlink entry rejected"));
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // replace_consensus_store via local HTTP server (wiremock)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn replace_consensus_store_fetches_and_extracts() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let data = build_tar_lz4(&[("store.db", b"consensus-data")])?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/snapshot.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("dest");
        let tmp = dir.path().join("tmp");
        let url = format!("{}/snapshot.tar.lz4", server.uri());

        tokio::task::spawn_blocking(move || replace_consensus_store(&url, &dest, &tmp)).await??;

        assert!(dir.path().join("dest/store.db").exists());
        assert!(dir.path().join("dest").join(SNAPSHOT_VERSION_FILE).exists());
        // tmp dir should be cleaned up
        assert!(!dir.path().join("tmp").exists());
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // restore_consensus
    // ---------------------------------------------------------------------------

    /// Serves one `.tar.lz4` archive containing `store.db` and returns its URL.
    async fn serve_consensus_archive(server: &wiremock::MockServer) -> Result<String> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let data = build_tar_lz4(&[("store.db", b"fresh-consensus-data")])?;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string().as_str()),
            )
            .mount(server)
            .await;
        Ok(format!("{}/cl.tar.lz4", server.uri()))
    }

    #[tokio::test]
    async fn restore_consensus_skips_when_the_marker_matches() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let consensus = dir.path().join("consensus");
        std::fs::create_dir_all(&consensus)?;
        std::fs::write(consensus.join("store.db"), b"existing")?;
        write_snapshot_version(&consensus, "http://unreachable.invalid/cl.tar.lz4")?;

        // No mock server: reaching the network would fail the test.
        restore_consensus(
            "http://unreachable.invalid/cl.tar.lz4",
            &consensus,
            &dir.path().join("tmp"),
            false,
        )?;

        assert_eq!(std::fs::read(consensus.join("store.db"))?, b"existing");
        Ok(())
    }

    #[tokio::test]
    async fn restore_consensus_keeps_the_malachite_home_under_force() -> Result<()> {
        // The consensus directory is the malachite home. --force replaces the
        // store, but deleting the directory would take the validator's private
        // key and the consensus WAL with it — see `restore_consensus`.
        let server = wiremock::MockServer::start().await;
        let url = serve_consensus_archive(&server).await?;

        let dir = tempfile::tempdir()?;
        let consensus = dir.path().join("consensus");
        std::fs::create_dir_all(consensus.join("config"))?;
        std::fs::create_dir_all(consensus.join("wal"))?;
        std::fs::write(consensus.join("store.db"), b"stale")?;
        std::fs::write(
            consensus.join("config/priv_validator_key.json"),
            b"validator-key",
        )?;
        std::fs::write(consensus.join("wal/consensus.wal"), b"wal-entries")?;
        let tmp = dir.path().join("tmp");

        let (consensus_arg, url_arg) = (consensus.clone(), url.clone());
        tokio::task::spawn_blocking(move || {
            restore_consensus(&url_arg, &consensus_arg, &tmp, true)
        })
        .await??;

        // The store is replaced...
        assert_eq!(
            std::fs::read(consensus.join("store.db"))?,
            b"fresh-consensus-data"
        );
        assert_eq!(
            std::fs::read_to_string(consensus.join(SNAPSHOT_VERSION_FILE))?,
            url
        );
        // ...and the node's identity survives.
        assert_eq!(
            std::fs::read(consensus.join("config/priv_validator_key.json"))?,
            b"validator-key"
        );
        // So does the WAL. Malachite wipes it when its height does not match the
        // one the restored node starts at, and replays it when it does — and that
        // replay is what makes the node re-cast the vote it cast before.
        assert_eq!(
            std::fs::read(consensus.join("wal/consensus.wal"))?,
            b"wal-entries"
        );
        Ok(())
    }

    #[test]
    fn url_identity_drops_a_regenerated_signature() {
        // Keeping one would make every freshly signed URL for the same snapshot
        // read as a new snapshot and re-fetch the whole layer.
        let bare = "https://x.example/testnet/manifest.json";
        for query in [
            "X-Amz-Signature=deadbeef&X-Amz-Date=20260813T000000Z",
            "x-amz-signature=deadbeef",
            "X-Goog-Signature=deadbeef",
            "AWSAccessKeyId=AKIA&Expires=1&Signature=deadbeef",
        ] {
            assert_eq!(url_identity(&format!("{bare}?{query}")), bare, "{query}");
        }
    }

    #[test]
    fn url_identity_keeps_a_parameter_that_addresses_the_snapshot() {
        // The failure this prevents is silent: two chains sharing one identity
        // means restoring either reports the other as up to date, and the node
        // starts on a datadir for the wrong network.
        let devnet = url_identity("https://x.example/manifest.json?network=arc-devnet");
        let testnet = url_identity("https://x.example/manifest.json?network=arc-testnet");
        assert_ne!(devnet, testnet);
        assert!(devnet.contains("network=arc-devnet"));

        // And a signature alongside one is still dropped.
        assert_eq!(
            url_identity("https://x.example/manifest.json?network=arc-devnet&X-Amz-Signature=dead"),
            devnet
        );
    }

    #[test]
    fn url_identity_ignores_parameter_order() {
        // A resolver reordering its parameters must not count as a new snapshot.
        assert_eq!(
            url_identity("https://x.example/m.json?a=1&b=2"),
            url_identity("https://x.example/m.json?b=2&a=1")
        );
    }

    #[test]
    fn url_identity_drops_the_fragment_and_normalizes_once() {
        // A fragment never reaches the server, so it addresses nothing.
        assert_eq!(
            url_identity("https://x.example/cl.tar.lz4#part"),
            "https://x.example/cl.tar.lz4"
        );
        assert_eq!(
            url_identity("https://x.example/m.json?network=devnet#part"),
            "https://x.example/m.json?network=devnet"
        );
        // Idempotent, so normalizing an already-composed marker is harmless.
        let once = url_identity("https://x.example/m.json?a=1");
        assert_eq!(url_identity(&once), once);
    }

    #[test]
    fn manifest_classification_survives_a_query_string() {
        // The identity now keeps semantic parameters, so classification has to
        // read the path — otherwise the last segment is "manifest.json?network=x"
        // and a manifest URL is restored as if it were a single archive.
        assert!(matches!(
            ExecutionSnapshotSource::from_url(
                "https://x.example/manifest.json?network=arc-devnet".to_string()
            ),
            ExecutionSnapshotSource::Manifest(_)
        ));
        assert!(matches!(
            ExecutionSnapshotSource::from_url(
                "https://x.example/el.tar.lz4?network=arc-devnet".to_string()
            ),
            ExecutionSnapshotSource::Archive(_)
        ));
    }

    #[tokio::test]
    async fn restore_consensus_ignores_a_changed_query_string() -> Result<()> {
        // A pre-signed URL is re-signed on every resolution. The marker records
        // the snapshot, not the signature, so the second run must skip rather
        // than re-download an identical archive.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let data = build_tar_lz4(&[("store.db", b"fresh-consensus-data")])?;
        let mock = Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(data.clone())
                    .append_header("Content-Length", data.len().to_string().as_str()),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let consensus = dir.path().join("consensus");
        let tmp = dir.path().join("tmp");
        let base = format!("{}/cl.tar.lz4", server.uri());

        for signature in ["aaa", "bbb"] {
            let url = format!("{base}?X-Amz-Signature={signature}");
            let (consensus_arg, tmp_arg) = (consensus.clone(), tmp.clone());
            tokio::task::spawn_blocking(move || {
                restore_consensus(&url, &consensus_arg, &tmp_arg, false)
            })
            .await??;
        }

        // No signature persisted, and only one download happened.
        assert_eq!(
            std::fs::read_to_string(consensus.join(SNAPSHOT_VERSION_FILE))?,
            base
        );
        drop(mock);
        Ok(())
    }

    #[tokio::test]
    async fn restore_consensus_cleans_up_the_staging_directory() -> Result<()> {
        let server = wiremock::MockServer::start().await;
        let url = serve_consensus_archive(&server).await?;

        let dir = tempfile::tempdir()?;
        let consensus = dir.path().join("consensus");
        let tmp = dir.path().join(".snapshot-tmp");

        let (consensus_arg, tmp_arg) = (consensus.clone(), tmp.clone());
        tokio::task::spawn_blocking(move || {
            restore_consensus(&url, &consensus_arg, &tmp_arg, false)
        })
        .await??;

        assert!(consensus.join("store.db").exists());
        // Both the staging subdirectory and its parent are removed.
        assert!(!tmp.exists());
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_fetches_el_and_cl() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"el-data")])?;
        let cl_data = build_tar_lz4(&[("store.db", b"cl-data")])?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/el.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(cl_data.clone())
                    .append_header("Content-Length", cl_data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = format!("{}/cl.tar.lz4", server.uri());
        let el_url_clone = el_url.clone();
        let cl_url_clone = cl_url.clone();

        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest, &cl_dest, &tmp, false)
        })
        .await??;

        assert!(dir.path().join("el/db/mdbx.dat").exists());
        assert!(dir.path().join("cl/store.db").exists());
        assert!(!dir.path().join("tmp").exists());
        // Version markers should be written
        assert_eq!(
            std::fs::read_to_string(dir.path().join("el/.snapshot-url"))?,
            el_url_clone
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cl/.snapshot-url"))?,
            cl_url_clone
        );
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_replaces_a_manifest_shaped_datadir() -> Result<()> {
        // The fixture archive names only db/, standing in for the general case:
        // extraction leaves whatever the archive does not name, so a datadir from
        // an earlier restore has to be removed rather than written over.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"archive-el")])?;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/el.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = "http://unreachable.invalid/cl.tar.lz4".to_string();

        // A datadir a manifest restore left: extra component directories, and a
        // marker naming the manifest so the archive URL is a mismatch.
        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::create_dir_all(el_dest.join("static_files"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"manifest-el")?;
        std::fs::write(el_dest.join("static_files/headers.jar"), b"stale")?;
        std::fs::write(el_dest.join("reth.toml"), b"stale")?;
        write_snapshot_version(&el_dest, "http://x/manifest.json el-profile=full")?;
        // The consensus layer is already current, so it stays off the network.
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"cl")?;
        write_snapshot_version(&cl_dest, &cl_url)?;

        let (el_dest_arg, cl_dest_arg) = (el_dest.clone(), cl_dest.clone());
        let el_url_arg = el_url.clone();
        tokio::task::spawn_blocking(move || {
            download_and_extract_both(
                &el_url_arg,
                &cl_url,
                &el_dest_arg,
                &cl_dest_arg,
                &tmp,
                false,
            )
        })
        .await??;

        assert_eq!(std::fs::read(el_dest.join("db/mdbx.dat"))?, b"archive-el");
        assert!(
            !el_dest.join("static_files/headers.jar").exists(),
            "a manifest restore's components must not survive an archive restore"
        );
        assert!(!el_dest.join("reth.toml").exists());
        assert_eq!(
            std::fs::read_to_string(el_dest.join(SNAPSHOT_VERSION_FILE))?,
            el_url
        );
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_skips_existing() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"el-data")])?;
        let cl_data = build_tar_lz4(&[("store.db", b"cl-data")])?;

        let server = MockServer::start().await;
        let el_mock = Mock::given(method("GET"))
            .and(path("/el.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .expect(0)
            .mount_as_scoped(&server)
            .await;
        let cl_mock = Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(cl_data.clone())
                    .append_header("Content-Length", cl_data.len().to_string().as_str()),
            )
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = format!("{}/cl.tar.lz4", server.uri());

        // Pre-populate dest dirs with data and matching version markers
        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"existing-el")?;
        std::fs::write(el_dest.join(SNAPSHOT_VERSION_FILE), &el_url)?;
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"existing-cl")?;
        std::fs::write(cl_dest.join(SNAPSHOT_VERSION_FILE), &cl_url)?;

        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest, &cl_dest, &tmp, false)
        })
        .await??;

        // Data should be untouched
        assert_eq!(
            std::fs::read(dir.path().join("el/db/mdbx.dat"))?,
            b"existing-el"
        );
        assert_eq!(
            std::fs::read(dir.path().join("cl/store.db"))?,
            b"existing-cl"
        );

        // Explicitly verify mocks received 0 requests
        drop(el_mock);
        drop(cl_mock);
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_force_overrides_skip() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"new-el")])?;
        let cl_data = build_tar_lz4(&[("store.db", b"new-cl")])?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/el.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(cl_data.clone())
                    .append_header("Content-Length", cl_data.len().to_string().as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = format!("{}/cl.tar.lz4", server.uri());

        // Pre-populate dest dirs with old data and old markers
        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"old-el")?;
        std::fs::write(el_dest.join(SNAPSHOT_VERSION_FILE), "http://old/el.tar.lz4")?;
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"old-cl")?;
        std::fs::write(cl_dest.join(SNAPSHOT_VERSION_FILE), "http://old/cl.tar.lz4")?;

        let el_url_clone = el_url.clone();
        let cl_url_clone = cl_url.clone();

        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest, &cl_dest, &tmp, true)
        })
        .await??;

        // Data should be overwritten
        assert_eq!(std::fs::read(dir.path().join("el/db/mdbx.dat"))?, b"new-el");
        assert_eq!(std::fs::read(dir.path().join("cl/store.db"))?, b"new-cl");
        // Markers should be updated
        assert_eq!(
            std::fs::read_to_string(dir.path().join("el/.snapshot-url"))?,
            el_url_clone
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cl/.snapshot-url"))?,
            cl_url_clone
        );
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_force_keeps_the_validator_key() -> Result<()> {
        // Same rule as restore_consensus: the forced pair restore replaces both
        // stores but must not delete the malachite home around one of them.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"new-el")])?;
        let cl_data = build_tar_lz4(&[("store.db", b"new-cl")])?;

        let server = MockServer::start().await;
        for (route, body) in [("/el.tar.lz4", &el_data), ("/cl.tar.lz4", &cl_data)] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(body.clone())
                        .append_header("Content-Length", body.len().to_string().as_str()),
                )
                .mount(&server)
                .await;
        }

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = format!("{}/cl.tar.lz4", server.uri());

        std::fs::create_dir_all(cl_dest.join("config"))?;
        std::fs::write(
            cl_dest.join("config/priv_validator_key.json"),
            b"validator-key",
        )?;

        let (el_dest_arg, cl_dest_arg) = (el_dest.clone(), cl_dest.clone());
        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest_arg, &cl_dest_arg, &tmp, true)
        })
        .await??;

        assert_eq!(std::fs::read(el_dest.join("db/mdbx.dat"))?, b"new-el");
        assert_eq!(std::fs::read(cl_dest.join("store.db"))?, b"new-cl");
        assert_eq!(
            std::fs::read(cl_dest.join("config/priv_validator_key.json"))?,
            b"validator-key"
        );
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_redownloads_when_url_differs() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"new-el")])?;
        let cl_data = build_tar_lz4(&[("store.db", b"new-cl")])?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/el-v2.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cl-v2.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(cl_data.clone())
                    .append_header("Content-Length", cl_data.len().to_string().as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let tmp = dir.path().join("tmp");

        // Pre-populate with old data and old version markers
        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"old-el")?;
        std::fs::write(
            el_dest.join(SNAPSHOT_VERSION_FILE),
            "http://old/el-v1.tar.lz4",
        )?;
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"old-cl")?;
        std::fs::write(
            cl_dest.join(SNAPSHOT_VERSION_FILE),
            "http://old/cl-v1.tar.lz4",
        )?;

        // New URLs differ from markers
        let el_url = format!("{}/el-v2.tar.lz4", server.uri());
        let cl_url = format!("{}/cl-v2.tar.lz4", server.uri());
        let el_url_clone = el_url.clone();
        let cl_url_clone = cl_url.clone();

        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest, &cl_dest, &tmp, false)
        })
        .await??;

        // Data should be overwritten with new snapshot
        assert_eq!(std::fs::read(dir.path().join("el/db/mdbx.dat"))?, b"new-el");
        assert_eq!(std::fs::read(dir.path().join("cl/store.db"))?, b"new-cl");
        // Markers should reflect new URLs
        assert_eq!(
            std::fs::read_to_string(dir.path().join("el/.snapshot-url"))?,
            el_url_clone
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cl/.snapshot-url"))?,
            cl_url_clone
        );
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_refuses_data_it_did_not_restore() -> Result<()> {
        // A node that synced from genesis, or a validator signing since
        // `arc-node-consensus init`, has data and no marker. So does a restore
        // that died before writing one, and the two are indistinguishable.
        // You need to use the --force flag to replace them.
        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");

        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"self-synced")?;
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"own-votes")?;

        let (el_dest_arg, cl_dest_arg) = (el_dest.clone(), cl_dest.clone());
        let tmp = dir.path().join("tmp");
        let err = tokio::task::spawn_blocking(move || {
            download_and_extract_both(
                "http://unreachable.invalid/el.tar.lz4",
                "http://unreachable.invalid/cl.tar.lz4",
                &el_dest_arg,
                &cl_dest_arg,
                &tmp,
                false,
            )
        })
        .await?
        .expect_err("data with no marker must stop the run");

        assert!(err.to_string().contains("--force"), "unexpected: {err}");
        assert_eq!(std::fs::read(el_dest.join("db/mdbx.dat"))?, b"self-synced");
        assert_eq!(std::fs::read(cl_dest.join("store.db"))?, b"own-votes");
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_both_refuses_before_replacing_either_layer() -> Result<()> {
        // The execution marker names an older snapshot, so that layer is due to be
        // replaced, and its archive is served so the replacement would succeed.
        // The consensus layer is unmarked and refuses. That refusal has to be
        // decided first, or a rejected run still costs the operator the datadir.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let el_data = build_tar_lz4(&[("db/mdbx.dat", b"new-el")])?;
        let server = MockServer::start().await;
        let el_mock = Mock::given(method("GET"))
            .and(path("/el.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(el_data.clone())
                    .append_header("Content-Length", el_data.len().to_string().as_str()),
            )
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let el_dest = dir.path().join("el");
        let cl_dest = dir.path().join("cl");
        let el_url = format!("{}/el.tar.lz4", server.uri());
        let cl_url = format!("{}/cl.tar.lz4", server.uri());

        std::fs::create_dir_all(el_dest.join("db"))?;
        std::fs::write(el_dest.join("db/mdbx.dat"), b"old-el")?;
        write_snapshot_version(&el_dest, "http://x.example/older.tar.lz4")?;
        std::fs::create_dir_all(&cl_dest)?;
        std::fs::write(cl_dest.join("store.db"), b"own-votes")?;

        let (el_dest_arg, cl_dest_arg) = (el_dest.clone(), cl_dest.clone());
        let tmp = dir.path().join("tmp");
        let err = tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest_arg, &cl_dest_arg, &tmp, false)
        })
        .await?
        .expect_err("the consensus layer must refuse");

        assert_eq!(
            std::fs::read(el_dest.join("db/mdbx.dat"))?,
            b"old-el",
            "the execution layer was replaced before the consensus refusal: {err}"
        );
        // Not even fetched: the refusal precedes the download.
        drop(el_mock);
        Ok(())
    }

    #[tokio::test]
    async fn restore_consensus_withdraws_the_marker_before_extracting() -> Result<()> {
        // The consensus directory is never removed, so its marker outlives a
        // failed restore unless the restore withdraws it. Extraction truncates
        // `store.db` as it writes, and here the snapshot being restored is the one
        // the marker already names — a --force retry. Keep the marker and the next
        // run reads a truncated store as up to date.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let partial = build_tar_lz4_failing_after("store.db", b"half-written")?;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(partial.clone())
                    .append_header("Content-Length", partial.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let consensus = dir.path().join("consensus");
        std::fs::create_dir_all(&consensus)?;
        std::fs::write(consensus.join("store.db"), b"stale")?;
        let url = format!("{}/cl.tar.lz4", server.uri());
        write_snapshot_version(&consensus, &url)?;

        let (consensus_arg, url_arg) = (consensus.clone(), url.clone());
        let tmp = dir.path().join("tmp");
        let err = tokio::task::spawn_blocking(move || {
            restore_consensus(&url_arg, &consensus_arg, &tmp, true)
        })
        .await?
        .expect_err("a partial archive must not report success");

        assert!(!consensus.join(SNAPSHOT_VERSION_FILE).exists(), "{err}");
        let rerun = should_download("Consensus layer", &consensus, &url, true, false)
            .expect_err("a truncated store must not read as up to date");
        assert!(rerun.to_string().contains("--force"), "unexpected: {rerun}");
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Snapshot listing helpers + consensus URL resolver
    // ---------------------------------------------------------------------------

    fn snapshot_listing(entries: &[serde_json::Value]) -> String {
        serde_json::to_string(&serde_json::json!({
            "snapshots": [{ "ignored": true }],
            "v2Snapshots": entries,
        }))
        .unwrap()
    }

    fn v2_snapshot_entry(network: &str, retention: &str, block_number: u64) -> serde_json::Value {
        let prefix = format!("{network}/storage-v2/{retention}/{block_number}/");
        serde_json::json!({
            "network": network,
            "retention": retention,
            "blockNumber": block_number,
            "timestamp": "2026-08-27T12:20:00Z",
            "prefix": prefix,
            "execution": {
                "manifestKey": format!("{prefix}execution/manifest.json"),
                "components": [{ "name": "state", "size": 55864490224_u64 }],
            },
            "consensus": {
                "key": format!("{prefix}consensus.tar.lz4"),
            },
        })
    }

    #[tokio::test]
    async fn fetch_latest_consensus_url_returns_latest_complete_v2_entry() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            v2_snapshot_entry("testnet", "archive", 100),
            v2_snapshot_entry("testnet", "archive", 200),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let url = fetch_latest_consensus_url_from(Chain::Testnet, &server.uri()).await?;

        assert_eq!(
            url,
            format!(
                "{}/download/testnet/storage-v2/archive/200/consensus.tar.lz4",
                server.uri()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_consensus_url_filters_by_network() -> Result<()> {
        // The listing API returns entries for every network; only the requested
        // network's consensus snapshot may be selected, even when another
        // network has a higher block.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            v2_snapshot_entry("devnet", "archive", 99999),
            v2_snapshot_entry("testnet", "archive", 100),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let url = fetch_latest_consensus_url_from(Chain::Testnet, &server.uri()).await?;

        assert!(
            url.contains("testnet/storage-v2/archive/100"),
            "devnet entry must not be selected; got {url}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn snapshot_listing_queries_the_bare_network_name() {
        // The API's ?network= takes `devnet`, not `arc-devnet`. Each mock answers
        // only the bare name, so rendering the reth-facing value here would 404
        // and fail the call — which is how this would otherwise break: silently.
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for (chain, network) in [
            (Chain::Testnet, "testnet"),
            (Chain::Devnet, "devnet"),
            (Chain::Mainnet, "mainnet"),
        ] {
            let server = MockServer::start().await;
            let body = snapshot_listing(&[v2_snapshot_entry(network, "archive", 1)]);
            Mock::given(method("GET"))
                .and(path("/snapshots"))
                .and(query_param("network", network))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;

            assert!(
                fetch_latest_consensus_url_from(chain, &server.uri())
                    .await
                    .is_ok(),
                "{chain} must query network={network}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_latest_consensus_url_propagates_http_error() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = fetch_latest_consensus_url_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err();

        // reqwest's error_for_status surfaces the HTTP status code.
        assert!(err.to_string().contains("500"), "unexpected error: {err}");
        Ok(())
    }

    #[tokio::test]
    async fn replace_consensus_store_cleans_tmp_on_extraction_failure() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Serve a corrupted archive that will fail extraction (symlink entry)
        let buf = Vec::new();
        let encoder = lz4::EncoderBuilder::new().build(buf)?;
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.as_gnu_mut().unwrap().linkname[..b"/etc\0".len()].copy_from_slice(b"/etc\0");
        let name_bytes = b"link\0";
        header.as_gnu_mut().unwrap().name[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder.append(&header, b"".as_ref())?;
        let (evil_data, result) = builder.into_inner()?.finish();
        result?;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/bad.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(evil_data.clone())
                    .append_header("Content-Length", evil_data.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("dest");
        let tmp = dir.path().join("tmp");
        let url = format!("{}/bad.tar.lz4", server.uri());

        let result =
            tokio::task::spawn_blocking(move || replace_consensus_store(&url, &dest, &tmp)).await?;

        assert!(result.is_err());
        // tmp should be cleaned up even on failure
        assert!(!dir.path().join("tmp").exists());
        Ok(())
    }
}
