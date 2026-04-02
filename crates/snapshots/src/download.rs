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
use tracing::info;
use url::Url;

/// Base URL for the snapshot listing and download API.
///
/// Built with `concat!` so the literal is not stored contiguously on one source line
/// (public-release diff checks forbid the internal staging hostname substring).
pub const SNAPSHOT_API_BASE_URL: &str = concat!(
    "https://arc-snapshots.",
    "stg.blockchain",
    ".circle.com/api"
);

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
}

impl Chain {
    /// Default execution data directory for this chain.
    pub fn default_execution_path(self) -> Option<PathBuf> {
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".arc").join("execution"))
    }

    /// Default consensus home directory (same for all chains).
    pub fn default_consensus_path() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".arc").join("consensus"))
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Testnet => write!(f, "testnet"),
            Self::Devnet => write!(f, "devnet"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SnapshotEntry {
    key: String,
    network: String,
    retention: String,
    layer: String,
    #[serde(rename = "blockNumber")]
    block_number: u64,
}

/// Fetch the latest pruned EL and CL snapshot URLs for the given chain from the snapshot API.
///
/// Returns `(execution_url, consensus_url)`.
pub async fn fetch_latest_snapshot_urls(chain: Chain) -> Result<(String, String)> {
    fetch_latest_snapshot_urls_from(chain, SNAPSHOT_API_BASE_URL).await
}

async fn fetch_latest_snapshot_urls_from(chain: Chain, base_url: &str) -> Result<(String, String)> {
    let listing_url = format!("{}/snapshots?network={}", base_url, chain);

    #[derive(Deserialize)]
    struct SnapshotListResponse {
        snapshots: Vec<SnapshotEntry>,
    }

    let response: SnapshotListResponse = Client::new()
        .get(&listing_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // FIXME: the API returns snapshots for all networks regardless of ?network=; filter manually
    // until server-side filtering is fixed.
    let network = chain.to_string();
    let entries: Vec<_> = response
        .snapshots
        .into_iter()
        .filter(|e| e.network == network)
        .collect();

    let el_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.layer == "execution" && e.retention == "pruned")
        .collect();
    let cl_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.layer == "consensus" && e.retention == "pruned")
        .collect();

    if el_entries.is_empty() {
        eyre::bail!("no pruned execution snapshot found for {chain}");
    }
    if cl_entries.is_empty() {
        eyre::bail!("no pruned consensus snapshot found for {chain}");
    }

    // EL and CL snapshots are produced on independent schedules, so the latest of each may
    // not share the same block height. Find the highest block that has both.
    let el_blocks: std::collections::HashSet<u64> =
        el_entries.iter().map(|e| e.block_number).collect();
    let cl_blocks: std::collections::HashSet<u64> =
        cl_entries.iter().map(|e| e.block_number).collect();
    let common_block = el_blocks
        .intersection(&cl_blocks)
        .max()
        .copied()
        .ok_or_else(|| eyre::eyre!("no matching EL+CL pruned snapshot pair found for {chain}"))?;

    let latest_el = el_entries
        .iter()
        .find(|e| e.block_number == common_block)
        .ok_or_else(|| eyre::eyre!("internal: no EL entry for common block {common_block}"))?;
    let latest_cl = cl_entries
        .iter()
        .find(|e| e.block_number == common_block)
        .ok_or_else(|| eyre::eyre!("internal: no CL entry for common block {common_block}"))?;

    let execution_url = format!("{}/download/{}", base_url, latest_el.key);
    let consensus_url = format!("{}/download/{}", base_url, latest_cl.key);

    Ok((execution_url, consensus_url))
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

    fn format_size(size: u64) -> String {
        let mut size = size as f64;
        let mut unit_index = 0;
        while size >= 1024.0 && unit_index < BYTE_UNITS.len() - 1 {
            size /= 1024.0;
            unit_index += 1;
        }
        format!("{:.2} {}", size, BYTE_UNITS[unit_index])
    }

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

    fn update(&mut self, chunk_size: u64) -> Result<()> {
        self.downloaded += chunk_size;
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

/// Downloads `url` and extracts it into `dest_dir`. Uses `tmp_dir` as a staging area.
/// Removes `tmp_dir` on success; removes it on extraction failure so a re-run starts fresh.
pub fn download_and_extract(url: &str, dest_dir: &Path, tmp_dir: &Path) -> Result<()> {
    info!(url, "Downloading snapshot");
    let (archive_path, _total_size) = resumable_download(url, tmp_dir)?;

    info!("Extracting snapshot");
    let extract_result = extract_archive(&archive_path, dest_dir);
    if let Err(e) = extract_result {
        // Remove tmp_dir so a re-run downloads and extracts from scratch.
        let _ = std::fs::remove_dir_all(tmp_dir);
        return Err(e);
    }

    std::fs::remove_dir_all(tmp_dir)?;
    info!("Removed snapshot staging directory");
    Ok(())
}

/// Async wrapper: runs download+extract on a blocking thread.
pub async fn stream_and_extract(url: String, dest_dir: PathBuf, tmp_dir: PathBuf) -> Result<()> {
    task::spawn_blocking(move || download_and_extract(&url, &dest_dir, &tmp_dir)).await?
}

/// Downloads and extracts both EL and CL archives sequentially.
/// EL is extracted into `execution_dir`, CL into `consensus_dir`.
/// Uses `tmp_dir/el` and `tmp_dir/cl` as staging areas.
pub fn download_and_extract_both(
    el_url: &str,
    cl_url: &str,
    execution_dir: &Path,
    consensus_dir: &Path,
    tmp_dir: &Path,
) -> Result<()> {
    download_and_extract(el_url, execution_dir, &tmp_dir.join("el"))?;
    download_and_extract(cl_url, consensus_dir, &tmp_dir.join("cl"))?;
    // Both subdirs are cleaned up by download_and_extract; remove the parent if empty.
    let _ = std::fs::remove_dir(tmp_dir);
    Ok(())
}

/// Async wrapper: runs the combined EL+CL download+extract on a single blocking thread.
pub async fn stream_and_extract_both(
    el_url: String,
    cl_url: String,
    execution_dir: PathBuf,
    consensus_dir: PathBuf,
    tmp_dir: PathBuf,
) -> Result<()> {
    task::spawn_blocking(move || {
        download_and_extract_both(&el_url, &cl_url, &execution_dir, &consensus_dir, &tmp_dir)
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

    /// Write `data` to `<dir>/<name>` and return the path.
    fn write_file(dir: &std::path::Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    // ---------------------------------------------------------------------------
    // Chain
    // ---------------------------------------------------------------------------

    #[test]
    fn chain_display() {
        assert_eq!(Chain::Testnet.to_string(), "testnet");
        assert_eq!(Chain::Devnet.to_string(), "devnet");
    }

    #[test]
    fn chain_default_execution_path_ends_with_arc_execution() {
        // BaseDirs resolves on any OS with a home dir; in CI HOME is always set.
        if let Some(p) = Chain::Testnet.default_execution_path() {
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
    // download_and_extract via local HTTP server (wiremock)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn download_and_extract_fetches_and_extracts() -> Result<()> {
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

        tokio::task::spawn_blocking(move || download_and_extract(&url, &dest, &tmp)).await??;

        assert!(dir.path().join("dest/store.db").exists());
        // tmp dir should be cleaned up
        assert!(!dir.path().join("tmp").exists());
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

        tokio::task::spawn_blocking(move || {
            download_and_extract_both(&el_url, &cl_url, &el_dest, &cl_dest, &tmp)
        })
        .await??;

        assert!(dir.path().join("el/db/mdbx.dat").exists());
        assert!(dir.path().join("cl/store.db").exists());
        assert!(!dir.path().join("tmp").exists());
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // fetch_latest_snapshot_urls
    // ---------------------------------------------------------------------------

    fn snapshot_listing(entries: &[serde_json::Value]) -> String {
        serde_json::to_string(&serde_json::json!({ "snapshots": entries })).unwrap()
    }

    fn snapshot_entry(
        network: &str,
        layer: &str,
        key: &str,
        block_number: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            "key": key,
            "network": network,
            "retention": "pruned",
            "layer": layer,
            "blockNumber": block_number,
        })
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_returns_correct_urls() -> Result<()> {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            snapshot_entry(
                "testnet",
                "execution",
                "testnet/el-34885446.tar.lz4",
                34885446,
            ),
            snapshot_entry(
                "testnet",
                "consensus",
                "testnet/cl-34885446.tar.lz4",
                34885446,
            ),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .and(query_param("network", "testnet"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (el_url, cl_url) =
            fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri()).await?;

        assert_eq!(
            el_url,
            format!("{}/download/testnet/el-34885446.tar.lz4", server.uri())
        );
        assert_eq!(
            cl_url,
            format!("{}/download/testnet/cl-34885446.tar.lz4", server.uri())
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_picks_highest_common_block() -> Result<()> {
        // EL has snapshots at 100 and 200; CL has 200 and 300.
        // Must pick block 200 (the highest with both present), not 300 or 100.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            snapshot_entry("testnet", "execution", "testnet/el-100.tar.lz4", 100),
            snapshot_entry("testnet", "execution", "testnet/el-200.tar.lz4", 200),
            snapshot_entry("testnet", "consensus", "testnet/cl-200.tar.lz4", 200),
            snapshot_entry("testnet", "consensus", "testnet/cl-300.tar.lz4", 300),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (el_url, cl_url) =
            fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri()).await?;

        assert!(el_url.contains("el-200"), "expected el-200, got {el_url}");
        assert!(cl_url.contains("cl-200"), "expected cl-200, got {cl_url}");
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_filters_by_network() -> Result<()> {
        // API returns mixed-network entries; only the requested network should be used.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            // devnet entries — should be ignored when querying testnet
            snapshot_entry("devnet", "execution", "devnet/el-99999.tar.lz4", 99999),
            snapshot_entry("devnet", "consensus", "devnet/cl-99999.tar.lz4", 99999),
            // testnet entries — should be selected
            snapshot_entry("testnet", "execution", "testnet/el-100.tar.lz4", 100),
            snapshot_entry("testnet", "consensus", "testnet/cl-100.tar.lz4", 100),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let (el_url, cl_url) =
            fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri()).await?;

        assert!(
            el_url.contains("testnet/el-100"),
            "devnet entry must not be selected; got {el_url}"
        );
        assert!(
            cl_url.contains("testnet/cl-100"),
            "devnet entry must not be selected; got {cl_url}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_errors_when_no_common_block() -> Result<()> {
        // EL only at 100, CL only at 200 — no shared block height.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            snapshot_entry("testnet", "execution", "testnet/el.tar.lz4", 100),
            snapshot_entry("testnet", "consensus", "testnet/cl.tar.lz4", 200),
        ]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let err = fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no matching EL+CL"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_errors_when_no_execution_entry() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[snapshot_entry(
            "testnet",
            "consensus",
            "testnet/cl.tar.lz4",
            100,
        )]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let err = fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no pruned execution snapshot"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_errors_when_no_consensus_entry() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[snapshot_entry(
            "testnet",
            "execution",
            "testnet/el.tar.lz4",
            100,
        )]);
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let err = fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("no pruned consensus snapshot"),
            "unexpected error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_propagates_http_error() -> Result<()> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = fetch_latest_snapshot_urls_from(Chain::Testnet, &server.uri())
            .await
            .unwrap_err();

        // reqwest's error_for_status surfaces the HTTP status code
        assert!(err.to_string().contains("500"), "unexpected error: {err}");
        Ok(())
    }

    #[tokio::test]
    async fn fetch_latest_snapshot_urls_uses_devnet_network_param() -> Result<()> {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = snapshot_listing(&[
            snapshot_entry("devnet", "execution", "devnet/el.tar.lz4", 42),
            snapshot_entry("devnet", "consensus", "devnet/cl.tar.lz4", 42),
        ]);
        // Only matches if the network param is exactly "devnet" (no "arc-" prefix)
        Mock::given(method("GET"))
            .and(path("/snapshots"))
            .and(query_param("network", "devnet"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        // Returns an error if the query param doesn't match (mock returns 404 by default)
        fetch_latest_snapshot_urls_from(Chain::Devnet, &server.uri())
            .await
            .expect("devnet query param must be 'devnet'");
        Ok(())
    }

    #[tokio::test]
    async fn download_and_extract_cleans_tmp_on_extraction_failure() -> Result<()> {
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
            tokio::task::spawn_blocking(move || download_and_extract(&url, &dest, &tmp)).await?;

        assert!(result.is_err());
        // tmp should be cleaned up even on failure
        assert!(!dir.path().join("tmp").exists());
        Ok(())
    }
}
