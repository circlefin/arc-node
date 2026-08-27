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

//! arc-snapshots — download and extract Arc node snapshots.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use eyre::Result;
use tokio::task;
use tracing::info;

use arc_snapshots::download::{self, Chain, ExecutionSnapshotSource};
use arc_snapshots::manifest::{self, ElProfile};

/// Environment variable that overrides the execution binary.
const EXECUTION_BINARY_ENV: &str = "ARC_EXECUTION_BINARY";

/// Default execution binary, resolved on PATH unless overridden.
const DEFAULT_EXECUTION_BINARY: &str = "arc-node-execution";

#[derive(Debug, Parser)]
#[command(
    name = "arc-snapshots",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Arc node snapshot utilities",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Download the latest snapshot and extract EL and CL data to their respective directories.
    Download(DownloadArgs),
}

/// Download Arc node snapshots.
///
/// Downloads separate EL and CL archives and extracts them:
///   - execution archive (bare paths, e.g. db/) → --execution-path
///   - consensus archive (bare paths, e.g. store.db) → --consensus-path
#[derive(Debug, Parser)]
struct DownloadArgs {
    /// URL of the execution layer snapshot archive.
    ///
    /// Give this together with --consensus-url, or omit both to fetch the latest
    /// matched pair for --chain.
    #[arg(long)]
    execution_url: Option<String>,

    /// URL of the consensus layer snapshot archive.
    ///
    /// Give this together with --execution-url, or omit both to fetch the latest
    /// matched pair for --chain.
    #[arg(long)]
    consensus_url: Option<String>,

    /// Network to download a snapshot for.
    ///
    /// Required when a snapshot URL is not given, since it selects the latest
    /// snapshot from the API. Also required when the execution snapshot is a
    /// manifest url: arc-node-execution uses it to select the chainspec.
    #[arg(long)]
    chain: Option<Chain>,

    /// Directory to extract execution layer data into.
    ///
    /// Defaults to ~/.arc/execution.
    #[arg(long)]
    execution_path: Option<PathBuf>,

    /// Directory to extract consensus layer data into.
    ///
    /// Defaults to ~/.arc/consensus.
    #[arg(long)]
    consensus_path: Option<PathBuf>,

    /// Restore both layers whatever they already hold.
    ///
    /// Without this, a layer holding the requested snapshot is left alone, and a
    /// layer holding data that no restore recorded stops the run.
    ///
    /// This changes only *whether* a layer is restored, not how. Within one layer,
    /// a restore does not leave files from an older snapshot beside the new one.
    ///
    /// The layers are restored in sequence, so an interrupted run can leave them
    /// at different stages. Markers make that detectable rather than self-healing:
    /// a layer holding data with no marker is not overwritten by a restore
    /// unless the operator passes `--force`.
    #[arg(long = "force")]
    force_redownload: bool,

    /// Execution component preset for a manifest download: minimal, full, or
    /// archive.
    ///
    /// Defaults to minimal. The preset applies only when the execution snapshot
    /// is a manifest.
    #[arg(long, value_enum, default_value_t = ElProfile::Minimal)]
    el_profile: ElProfile,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Basic tracing to stdout
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Download(args) => run_download(args).await,
    }
}

/// The directories a restore writes to: the execution and consensus targets
/// and the staging directory downloads land in first.
struct SnapshotDirs {
    execution: PathBuf,
    consensus: PathBuf,
    tmp: PathBuf,
}

pub(crate) async fn run_download(args: DownloadArgs) -> Result<()> {
    let execution = args
        .execution_path
        .or_else(Chain::default_execution_path)
        .ok_or_else(|| {
            eyre::eyre!("Could not determine default execution path; use --execution-path")
        })?;
    let consensus = args
        .consensus_path
        .or_else(Chain::default_consensus_path)
        .ok_or_else(|| {
            eyre::eyre!("Could not determine default consensus path; use --consensus-path")
        })?;
    let tmp = snapshot_tmp_dir(&execution, &consensus)?;
    let dirs = SnapshotDirs {
        execution,
        consensus,
        tmp,
    };

    let (source, consensus_url) =
        resolve_sources(args.execution_url, args.consensus_url, args.chain).await?;

    match source {
        ExecutionSnapshotSource::Archive(execution_url) => {
            run_download_archive_snapshot(
                execution_url,
                consensus_url,
                dirs,
                args.force_redownload,
            )
            .await?;
        }
        ExecutionSnapshotSource::Manifest(manifest_url) => {
            let chain = args
                .chain
                .ok_or_else(|| eyre::eyre!("--chain is required for a manifest download"))?;
            let restore = ManifestRestore {
                chain,
                profile: args.el_profile,
                manifest_url,
                consensus_url,
                dirs,
                force_redownload: args.force_redownload,
                binary: execution_binary()?,
            };
            run_download_manifest_snapshot(restore, manifest::CommandDownloader).await?;
        }
    }

    info!("Snapshot operation complete");
    Ok(())
}

/// Restores both layers from single lz4 archives with the native path.
async fn run_download_archive_snapshot(
    execution_url: String,
    consensus_url: String,
    dirs: SnapshotDirs,
    force_redownload: bool,
) -> Result<()> {
    info!(
        execution_url = %execution_url,
        consensus_url = %consensus_url,
        execution_dir = %dirs.execution.display(),
        consensus_dir = %dirs.consensus.display(),
        "Starting snapshot download"
    );
    download::stream_and_extract_both(
        execution_url,
        consensus_url,
        dirs.execution,
        dirs.consensus,
        dirs.tmp,
        force_redownload,
    )
    .await
}

/// A manifest restore: which snapshot, where it goes, and how much of the
/// execution layer to fetch.
///
/// A struct rather than a parameter list because `manifest_url` and
/// `consensus_url` are both `String`: swapping them at a call site would
/// compile and hand each URL to the wrong layer.
struct ManifestRestore {
    /// Chain passed to `arc-node-execution download --chain`.
    chain: Chain,
    /// Execution component preset to fetch.
    profile: ElProfile,
    /// URL of the reth `manifest.json`.
    manifest_url: String,
    /// URL of the consensus `.tar.lz4` archive.
    consensus_url: String,
    /// Restore targets and the staging directory.
    dirs: SnapshotDirs,
    /// Whether to discard existing data instead of skipping up-to-date layers.
    force_redownload: bool,
    /// Execution binary the manifest is handed to.
    binary: OsString,
}

/// Restores the execution layer from a reth manifest with
/// `arc-node-execution download` and the consensus layer with the native path.
///
/// `downloader` is a parameter so tests can record the invocation instead of
/// launching a process.
async fn run_download_manifest_snapshot<D>(restore: ManifestRestore, downloader: D) -> Result<()>
where
    D: manifest::ExecutionDownloader + Send + 'static,
{
    let ManifestRestore {
        chain,
        profile,
        manifest_url,
        consensus_url,
        dirs,
        force_redownload,
        binary,
    } = restore;

    let marker = manifest::manifest_marker(&manifest_url, profile);
    // Before the consensus restore, so a datadir this tool must not touch stops
    // the run rather than aborting it with one layer already replaced.
    let restore_execution = download::should_download(
        "Execution layer",
        &dirs.execution,
        &marker,
        download::execution_snapshot_exists(&dirs.execution),
        force_redownload,
    )?;
    if restore_execution {
        // reth writes into the datadir in place, so the directory is deleted
        // before the download.
        // Checking that the binary is there before deleting the existing data.
        downloader.ensure_available(&binary)?;
    }

    info!(
        chain = %chain,
        manifest_url = %manifest_url,
        execution_dir = %dirs.execution.display(),
        consensus_dir = %dirs.consensus.display(),
        "Starting snapshot download"
    );

    download::stream_restore_consensus(
        consensus_url,
        dirs.consensus.clone(),
        dirs.tmp.clone(),
        force_redownload,
    )
    .await?;

    if restore_execution {
        // reth writes into the datadir in place and writes only the files its
        // manifest lists, so whatever is already there survives and mixes with
        // the new data. Reaching this branch means the datadir is being
        // replaced — because --force says so, or because the marker names a
        // different snapshot — so it has to go first either way. There is no
        // staging alternative: the download lands directly in the datadir.
        manifest::clear_datadir(&dirs.execution)?;

        let el_dir = dirs.execution.clone();
        let el_url = manifest_url.clone();
        task::spawn_blocking(move || {
            let opts = manifest::ExecutionDownload {
                chain,
                profile,
                datadir: &el_dir,
                manifest_url: el_url.as_str(),
            };
            manifest::run_execution_download(&downloader, &binary, &opts)
        })
        .await??;

        // Reached only when the child exits zero, so an interrupted restore
        // leaves a datadir with no marker — which the next run refuses to touch
        // without --force rather than starting a node on part of a snapshot.
        download::write_snapshot_version(&dirs.execution, &marker)?;
    }

    Ok(())
}

/// Determines the execution snapshot source and consensus URL.
///
/// Explicit URLs are given together so both layers come from the same block. A
/// consensus snapshot from a different block leaves the node unable to hand off
/// between the layers. When both URLs are omitted, the latest matched pair for
/// `chain` is resolved from the API.
async fn resolve_sources(
    execution_url: Option<String>,
    consensus_url: Option<String>,
    chain: Option<Chain>,
) -> Result<(ExecutionSnapshotSource, String)> {
    match (execution_url, consensus_url) {
        (Some(el), Some(cl)) => Ok((ExecutionSnapshotSource::from_url(el), cl)),
        (Some(_), None) => eyre::bail!(
            "--execution-url requires --consensus-url; omit both to resolve a matched pair"
        ),
        (None, None) => {
            let chain = chain.ok_or_else(|| {
                eyre::eyre!(
                    "--chain is required when --execution-url and --consensus-url are not provided"
                )
            })?;
            info!(chain = %chain, "Fetching latest snapshot URLs");
            download::resolve_snapshot_sources(chain).await
        }
        (None, Some(_)) => eyre::bail!(
            "--consensus-url requires --execution-url; omit both to resolve a matched pair"
        ),
    }
}

/// The execution binary a manifest download is handed to.
///
/// Defaults to `arc-node-execution` on `PATH`; `ARC_EXECUTION_BINARY`
/// overrides it with another name or an absolute path.
fn execution_binary() -> Result<OsString> {
    resolve_execution_binary(std::env::var_os(EXECUTION_BINARY_ENV))
}

/// Applies the override rules to a raw `ARC_EXECUTION_BINARY` lookup. Split
/// from [`execution_binary`] so every outcome is testable without mutating the
/// process environment.
///
/// An unset variable falls back to the default. A variable that is set but holds
/// nothing usable is an error rather than a silent fallback, or the operator is
/// left wondering why the override did nothing — a blank entry in an env file is
/// how that happens.
///
/// The value stays an `OsString`: a path need not be UTF-8, and converting would
/// only add a failure mode for a configuration that works.
fn resolve_execution_binary(value: Option<OsString>) -> Result<OsString> {
    match value {
        None => Ok(OsString::from(DEFAULT_EXECUTION_BINARY)),
        Some(binary) if binary.to_string_lossy().trim().is_empty() => eyre::bail!(
            "{EXECUTION_BINARY_ENV} is set but empty; unset it to use \
             `{DEFAULT_EXECUTION_BINARY}` from PATH"
        ),
        Some(binary) => Ok(binary),
    }
}

/// Chooses a staging directory that survives forced target cleanup.
///
/// The snapshot archives must be downloaded outside both restore targets
/// because `--force` removes those targets before extraction. Prefer a
/// sibling of the execution path, then a sibling of the consensus path, and
/// fail if neither candidate is outside both targets.
fn snapshot_tmp_dir(execution_dir: &Path, consensus_dir: &Path) -> Result<PathBuf> {
    let execution_candidate = sibling_tmp_dir(execution_dir);
    if is_safe_tmp_dir(&execution_candidate, execution_dir, consensus_dir) {
        return Ok(execution_candidate);
    }

    let consensus_candidate = sibling_tmp_dir(consensus_dir);
    if is_safe_tmp_dir(&consensus_candidate, execution_dir, consensus_dir) {
        return Ok(consensus_candidate);
    }

    eyre::bail!(
        "could not derive a snapshot staging directory outside execution and consensus paths"
    )
}

/// Returns the conventional snapshot staging directory next to `dir`.
///
/// A sibling rather than a child because the execution restore removes its
/// target: an archive staged inside would be deleted before it could be
/// extracted. `arc-node-consensus download` stages inside its home instead, since
/// the consensus restore never removes anything.
fn sibling_tmp_dir(dir: &Path) -> PathBuf {
    dir.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(".snapshot-tmp")
}

/// Returns whether `candidate` is outside both restore target directories.
///
/// A safe staging directory must not be removed when the execution and
/// consensus directories are deleted during a clean forced restore.
fn is_safe_tmp_dir(candidate: &Path, execution_dir: &Path, consensus_dir: &Path) -> bool {
    !candidate.starts_with(execution_dir) && !candidate.starts_with(consensus_dir)
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    /// One recorded `arc-node-execution download` invocation.
    struct RecordedCall {
        binary: OsString,
        argv: Vec<OsString>,
        /// Whether the datadir still held state when the call arrived. Proves
        /// the forced delete happens before the hand-off, not after.
        datadir_had_state: bool,
    }

    /// How the fake behaves when a restore reaches it.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum FakeOutcome {
        /// Probe and download both succeed.
        Succeeds,
        /// The probe rejects the binary, standing in for one that is missing or
        /// too old for a manifest download.
        ProbeFails,
        /// The probe passes and the download fails partway, leaving behind the
        /// partial state reth extracts as it goes.
        RunFailsPartway,
        /// The download exits zero having written state but never finishing, so
        /// no `reth.toml` appears. reth's download returns success when the
        /// process is signalled, so a zero exit does not mean it completed.
        RunSucceedsWithoutFinishing,
    }

    /// Records how the execution binary would have been invoked instead of
    /// launching it.
    struct RecordingDownloader {
        calls: Arc<Mutex<Vec<RecordedCall>>>,
        outcome: FakeOutcome,
    }

    impl RecordingDownloader {
        fn new(outcome: FakeOutcome) -> (Self, Arc<Mutex<Vec<RecordedCall>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                    outcome,
                },
                calls,
            )
        }
    }

    impl manifest::ExecutionDownloader for RecordingDownloader {
        fn ensure_available(&self, binary: &OsStr) -> Result<()> {
            if self.outcome == FakeOutcome::ProbeFails {
                eyre::bail!(
                    "execution binary `{}` is unusable",
                    binary.to_string_lossy()
                );
            }
            Ok(())
        }

        fn run(&self, binary: &OsStr, argv: &[OsString]) -> Result<()> {
            let datadir = argv
                .iter()
                .position(|a| a == "--datadir")
                .and_then(|i| argv.get(i.saturating_add(1)))
                .map(PathBuf::from);
            self.calls.lock().unwrap().push(RecordedCall {
                binary: binary.to_os_string(),
                argv: argv.to_vec(),
                datadir_had_state: datadir
                    .as_deref()
                    .is_some_and(download::execution_snapshot_exists),
            });

            if let Some(datadir) = &datadir {
                // reth extracts into the datadir as it goes, so any run that got
                // started leaves state behind.
                std::fs::create_dir_all(datadir.join("db"))?;
                std::fs::write(datadir.join("db/mdbx.dat"), b"partial")?;
                // ...and writes reth.toml at the end, only once it has finished.
                if self.outcome == FakeOutcome::Succeeds {
                    std::fs::write(datadir.join("reth.toml"), b"[stages]")?;
                }
            }

            if self.outcome == FakeOutcome::RunFailsPartway {
                eyre::bail!("execution download failed");
            }
            Ok(())
        }
    }

    const TEST_MANIFEST_URL: &str = "http://x.example/testnet/manifest.json";
    const TEST_CONSENSUS_URL: &str = "http://unreachable.invalid/cl.tar.lz4";

    /// Builds a restore whose consensus layer is already up to date, so nothing
    /// in the test reaches the network.
    fn offline_restore(root: &Path, profile: ElProfile) -> ManifestRestore {
        let execution = root.join("execution");
        let consensus = root.join("consensus");
        std::fs::create_dir_all(&consensus).unwrap();
        std::fs::write(consensus.join("store.db"), b"cl").unwrap();
        download::write_snapshot_version(&consensus, TEST_CONSENSUS_URL).unwrap();

        ManifestRestore {
            chain: Chain::Testnet,
            profile,
            manifest_url: TEST_MANIFEST_URL.to_string(),
            consensus_url: TEST_CONSENSUS_URL.to_string(),
            dirs: SnapshotDirs {
                execution,
                consensus,
                tmp: root.join(".snapshot-tmp"),
            },
            force_redownload: false,
            binary: OsString::from("arc-node-execution"),
        }
    }

    /// Writes the `db/mdbx.dat` and marker an already-restored datadir has.
    fn seed_execution_dir(dir: &Path, marker: &str) {
        std::fs::create_dir_all(dir.join("db")).unwrap();
        std::fs::write(dir.join("db/mdbx.dat"), b"state").unwrap();
        download::write_snapshot_version(dir, marker).unwrap();
    }

    #[tokio::test]
    async fn manifest_restore_hands_the_manifest_to_the_execution_binary() {
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Full);
        let execution_dir = restore.dirs.execution.clone();
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].binary, OsStr::new("arc-node-execution"));
        assert_eq!(
            calls[0].argv,
            [
                OsString::from("download"),
                OsString::from("--chain"),
                OsString::from("arc-testnet"),
                OsString::from("--datadir"),
                OsString::from(&execution_dir),
                OsString::from("--full"),
                OsString::from("--manifest-url"),
                OsString::from(TEST_MANIFEST_URL),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(execution_dir.join(".snapshot-url")).unwrap(),
            manifest::manifest_marker(TEST_MANIFEST_URL, ElProfile::Full)
        );
    }

    #[tokio::test]
    async fn explicit_manifest_without_a_profile_uses_minimal() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "arc-testnet",
            "--execution-url",
            TEST_MANIFEST_URL,
            "--consensus-url",
            TEST_CONSENSUS_URL,
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), args.el_profile);
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].argv.contains(&OsString::from("--minimal")));
    }

    #[tokio::test]
    async fn explicit_archive_url_is_accepted_with_the_default_profile() {
        use tracing::instrument::WithSubscriber;

        let root = tempfile::tempdir().unwrap();
        let execution = root.path().join("execution");
        let consensus = root.path().join("consensus");
        let execution_url = "http://unreachable.invalid/el.tar.lz4";
        let consensus_url = "http://unreachable.invalid/cl.tar.lz4";
        std::fs::create_dir_all(execution.join("db")).unwrap();
        std::fs::write(execution.join("db/mdbx.dat"), b"el").unwrap();
        download::write_snapshot_version(&execution, execution_url).unwrap();
        std::fs::create_dir_all(&consensus).unwrap();
        std::fs::write(consensus.join("store.db"), b"cl").unwrap();
        download::write_snapshot_version(&consensus, consensus_url).unwrap();
        let args = DownloadArgs {
            execution_url: Some(execution_url.to_string()),
            consensus_url: Some(consensus_url.to_string()),
            chain: None,
            execution_path: Some(execution),
            consensus_path: Some(consensus),
            force_redownload: false,
            el_profile: ElProfile::Minimal,
        };
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer_bytes = Arc::clone(&bytes);
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || LogWriter(Arc::clone(&writer_bytes)))
            .finish();

        run_download(args)
            .with_subscriber(subscriber)
            .await
            .unwrap();

        assert!(bytes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn manifest_restore_skips_the_hand_off_when_the_marker_matches() {
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Minimal);
        seed_execution_dir(
            &restore.dirs.execution,
            &manifest::manifest_marker(TEST_MANIFEST_URL, ElProfile::Minimal),
        );
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn manifest_restore_repeats_the_hand_off_when_the_profile_changes() {
        // Same manifest URL, different profile: the component set differs, so
        // the restore must run rather than report the datadir as up to date.
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Archive);
        seed_execution_dir(
            &restore.dirs.execution,
            &manifest::manifest_marker(TEST_MANIFEST_URL, ElProfile::Minimal),
        );
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].argv.contains(&OsString::from("--archive")));
        // reth writes in place and only the files its manifest lists, so the
        // minimal-shaped datadir must be gone before the archive components
        // land on top of it.
        assert!(
            !calls[0].datadir_had_state,
            "a marker mismatch must clear the datadir before reth is invoked"
        );
    }

    #[tokio::test]
    async fn manifest_restore_writes_no_marker_when_the_hand_off_fails() {
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Minimal);
        let execution_dir = restore.dirs.execution.clone();
        let (downloader, _calls) = RecordingDownloader::new(FakeOutcome::RunFailsPartway);

        let err = run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("execution download failed"));
        assert!(!execution_dir.join(".snapshot-url").exists());
        // The datadir is left half-populated, which is the state a rerun has to
        // deal with: reth writes in place, so there is nothing to roll back.
        assert!(download::execution_snapshot_exists(&execution_dir));
    }

    #[tokio::test]
    async fn manifest_restore_records_nothing_when_the_hand_off_stops_early() {
        // The child exits zero without finishing, which is what reth does when it
        // is signalled. Trusting the exit code would write a marker over part of a
        // snapshot, and the next run would then skip the layer as up to date.
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Minimal);
        let execution_dir = restore.dirs.execution.clone();
        let (downloader, calls) =
            RecordingDownloader::new(FakeOutcome::RunSucceedsWithoutFinishing);

        let err = run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("reth.toml"), "unexpected: {err}");
        assert_eq!(calls.lock().unwrap().len(), 1, "the hand-off did happen");
        assert!(!execution_dir.join(".snapshot-url").exists());
    }

    #[tokio::test]
    async fn manifest_restore_refuses_a_datadir_it_did_not_restore() {
        // State and no marker: a node that synced from genesis, a datadir placed
        // by hand, or the hand-off above dying partway. Indistinguishable, so the
        // run stops and names --force rather than deleting one of them.
        let root = tempfile::tempdir().unwrap();
        let restore = offline_restore(root.path(), ElProfile::Minimal);
        let execution_dir = restore.dirs.execution.clone();
        std::fs::create_dir_all(execution_dir.join("db")).unwrap();
        std::fs::write(execution_dir.join("db/mdbx.dat"), b"self-synced").unwrap();
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        let err = run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("--force"), "unexpected: {err}");
        assert!(calls.lock().unwrap().is_empty(), "no hand-off may happen");
        assert_eq!(
            std::fs::read(execution_dir.join("db/mdbx.dat")).unwrap(),
            b"self-synced"
        );
    }

    #[tokio::test]
    async fn manifest_restore_needs_force_after_an_interrupted_hand_off() {
        // A hand-off that dies partway leaves state and no marker. The worst
        // outcome would be the next run reporting success over it, so the plain
        // rerun stops; --force is how the operator says to redo the restore.
        let root = tempfile::tempdir().unwrap();
        let (downloader, _calls) = RecordingDownloader::new(FakeOutcome::RunFailsPartway);
        let err = run_download_manifest_snapshot(
            offline_restore(root.path(), ElProfile::Minimal),
            downloader,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("execution download failed"));

        let restore = offline_restore(root.path(), ElProfile::Minimal);
        let execution_dir = restore.dirs.execution.clone();
        assert!(download::execution_snapshot_exists(&execution_dir));
        assert!(!execution_dir.join(".snapshot-url").exists());

        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);
        let err = run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--force"), "unexpected: {err}");
        assert!(calls.lock().unwrap().is_empty());

        // --force ignores the marker, so the consensus archive is really fetched.
        let (_server, consensus_url) = serve_consensus_archive().await;
        let mut restore = offline_restore(root.path(), ElProfile::Minimal);
        restore.consensus_url = consensus_url;
        restore.force_redownload = true;
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);
        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "--force must redo the restore"
        );
        assert_eq!(
            std::fs::read_to_string(execution_dir.join(".snapshot-url")).unwrap(),
            manifest::manifest_marker(TEST_MANIFEST_URL, ElProfile::Minimal)
        );
    }

    #[tokio::test]
    async fn manifest_restore_touches_nothing_when_the_probe_fails() {
        // --force is what makes this test bite: it is the mode that deletes the
        // datadir, so the probe has to reject the binary before that happens.
        let root = tempfile::tempdir().unwrap();
        let mut restore = offline_restore(root.path(), ElProfile::Minimal);
        restore.force_redownload = true;
        let (_server, consensus_url) = serve_consensus_archive().await;
        restore.consensus_url = consensus_url;
        let execution_dir = restore.dirs.execution.clone();
        let consensus_dir = restore.dirs.consensus.clone();
        seed_execution_dir(&execution_dir, "http://old/manifest.json");
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::ProbeFails);

        let err = run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("is unusable"), "unexpected: {err}");
        assert!(calls.lock().unwrap().is_empty(), "no hand-off may happen");
        // Both layers are as they were: an unusable binary must cost nothing.
        assert_eq!(
            std::fs::read(execution_dir.join("db/mdbx.dat")).unwrap(),
            b"state"
        );
        // The consensus marker, not its contents. The served archive holds the
        // same bytes the seed does, so only the marker separates "never
        // restored" from "wiped and restored again".
        assert_eq!(
            std::fs::read_to_string(consensus_dir.join(".snapshot-url")).unwrap(),
            TEST_CONSENSUS_URL
        );
    }

    #[tokio::test]
    async fn manifest_restore_clears_the_datadir_before_the_hand_off_under_force() {
        let root = tempfile::tempdir().unwrap();
        let mut restore = offline_restore(root.path(), ElProfile::Minimal);
        restore.force_redownload = true;
        // --force ignores the marker, so the consensus archive is really
        // downloaded; the server has to stay alive for the whole restore.
        let (_server, consensus_url) = serve_consensus_archive().await;
        restore.consensus_url = consensus_url;
        seed_execution_dir(&restore.dirs.execution, "http://old/manifest.json");
        let (downloader, calls) = RecordingDownloader::new(FakeOutcome::Succeeds);

        run_download_manifest_snapshot(restore, downloader)
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            !calls[0].datadir_had_state,
            "--force must clear the datadir before reth is invoked"
        );
    }

    /// Serves a minimal consensus archive from a local mock server. The server
    /// is returned so the caller can keep it alive.
    async fn serve_consensus_archive() -> (wiremock::MockServer, String) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let encoder = lz4::EncoderBuilder::new().build(Vec::new()).unwrap();
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(2);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "store.db", b"cl".as_ref())
            .unwrap();
        let (body, result) = builder.into_inner().unwrap().finish();
        result.unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cl.tar.lz4"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.clone())
                    .append_header("Content-Length", body.len().to_string().as_str()),
            )
            .mount(&server)
            .await;

        let url = format!("{}/cl.tar.lz4", server.uri());
        (server, url)
    }

    #[test]
    fn parse_download_with_explicit_paths() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--execution-url",
            "http://example.com/el.tar.lz4",
            "--consensus-url",
            "http://example.com/cl.tar.lz4",
            "--execution-path",
            "/tmp/el",
            "--consensus-path",
            "/tmp/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(
            args.execution_url.as_deref(),
            Some("http://example.com/el.tar.lz4")
        );
        assert_eq!(
            args.consensus_url.as_deref(),
            Some("http://example.com/cl.tar.lz4")
        );
        assert_eq!(args.execution_path, Some(PathBuf::from("/tmp/el")));
        assert_eq!(args.consensus_path, Some(PathBuf::from("/tmp/cl")));
    }

    #[test]
    fn parse_download_chain_default_is_none() {
        let cli = parse(&["arc-snapshots", "download"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert!(args.chain.is_none());
    }

    #[test]
    fn parse_download_explicit_chain_devnet() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "arc-devnet",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert!(matches!(args.chain, Some(Chain::Devnet)));
    }

    #[test]
    fn parse_download_explicit_chain_mainnet() {
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "arc-mainnet",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert!(matches!(args.chain, Some(Chain::Mainnet)));
    }

    #[test]
    fn parse_download_no_chain_with_urls_is_ok() {
        // Explicit URLs make --chain unnecessary — should parse cleanly.
        let cli = parse(&[
            "arc-snapshots",
            "download",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
            "--execution-path",
            "/tmp/el",
            "--consensus-path",
            "/tmp/cl",
        ])
        .unwrap();
        let Commands::Download(args) = cli.command;
        assert!(args.chain.is_none());
    }

    #[test]
    fn parse_download_bare_chain_name_is_error() {
        // "testnet" without the "arc-" prefix must be rejected
        assert!(parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "testnet",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .is_err());
    }

    #[test]
    fn parse_download_invalid_chain_is_error() {
        assert!(parse(&[
            "arc-snapshots",
            "download",
            "--chain",
            "not-a-chain",
            "--execution-url",
            "http://x/el",
            "--consensus-url",
            "http://x/cl",
        ])
        .is_err());
    }

    #[test]
    fn parse_no_subcommand_is_error() {
        assert!(parse(&["arc-snapshots"]).is_err());
    }

    #[test]
    fn parse_download_with_force_flag() {
        let cli = parse(&["arc-snapshots", "download", "--force"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert!(args.force_redownload);
    }

    #[test]
    fn parse_download_without_force_defaults_to_false() {
        let cli = parse(&["arc-snapshots", "download"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert!(!args.force_redownload);
    }

    #[test]
    fn parse_download_el_profile() {
        let cli = parse(&["arc-snapshots", "download"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(args.el_profile, ElProfile::Minimal);

        let cli = parse(&["arc-snapshots", "download", "--el-profile", "archive"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(args.el_profile, ElProfile::Archive);

        let cli = parse(&["arc-snapshots", "download", "--el-profile", "minimal"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(args.el_profile, ElProfile::Minimal);

        let cli = parse(&["arc-snapshots", "download", "--el-profile", "full"]).unwrap();
        let Commands::Download(args) = cli.command;
        assert_eq!(args.el_profile, ElProfile::Full);
    }

    #[test]
    fn download_help_advertises_the_minimal_profile_default() {
        use clap::CommandFactory;

        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("download")
            .unwrap()
            .render_long_help()
            .to_string();

        assert!(
            help.contains("[default: minimal]"),
            "unexpected help: {help}"
        );
    }

    #[test]
    fn resolve_execution_binary_prefers_the_override() {
        assert_eq!(
            resolve_execution_binary(Some(OsString::from("/opt/arc/bin/arc-node-execution")))
                .unwrap(),
            OsStr::new("/opt/arc/bin/arc-node-execution")
        );
    }

    #[test]
    fn resolve_execution_binary_falls_back_when_unset() {
        assert_eq!(
            resolve_execution_binary(None).unwrap(),
            OsStr::new(DEFAULT_EXECUTION_BINARY)
        );
    }

    #[test]
    fn resolve_execution_binary_rejects_an_empty_override() {
        // A blank env-file entry sets the variable without giving it a value.
        // Falling back silently would leave the operator wondering why their
        // override did nothing, and `Command::new("")` reports the binary as not
        // found — advising them to set a variable they did set.
        for blank in ["", "   "] {
            let err = resolve_execution_binary(Some(OsString::from(blank)))
                .unwrap_err()
                .to_string();
            assert!(err.contains("ARC_EXECUTION_BINARY"), "unexpected: {err}");
            assert!(err.contains("set but empty"), "unexpected: {err}");
        }
    }

    #[test]
    fn resolve_execution_binary_accepts_a_non_utf8_path() {
        // Paths are not required to be UTF-8, so an OsString override is passed
        // through rather than rejected.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let path = OsString::from_vec(b"/opt/arc/\xff/arc-node-execution".to_vec());
            assert_eq!(resolve_execution_binary(Some(path.clone())).unwrap(), path);
        }
    }

    #[test]
    fn snapshot_tmp_dir_uses_execution_parent() {
        let execution_dir = Path::new("/tmp/arc/execution");
        let consensus_dir = Path::new("/tmp/arc/consensus");
        let tmp_dir = snapshot_tmp_dir(execution_dir, consensus_dir).unwrap();

        assert_eq!(tmp_dir, PathBuf::from("/tmp/arc/.snapshot-tmp"));
        assert!(!tmp_dir.starts_with(execution_dir));
        assert!(!tmp_dir.starts_with(consensus_dir));
    }

    #[test]
    fn snapshot_tmp_dir_avoids_consensus_target() {
        let execution_dir = Path::new("/tmp/arc/execution");
        let consensus_dir = Path::new("/tmp/arc");
        let tmp_dir = snapshot_tmp_dir(execution_dir, consensus_dir).unwrap();

        assert_eq!(tmp_dir, PathBuf::from("/tmp/.snapshot-tmp"));
        assert!(!tmp_dir.starts_with(execution_dir));
        assert!(!tmp_dir.starts_with(consensus_dir));
    }

    #[test]
    fn snapshot_tmp_dir_errors_when_candidates_conflict() {
        let execution_dir = Path::new("/tmp/.snapshot-tmp");
        let consensus_dir = Path::new("/tmp/.snapshot-tmp");
        let err = snapshot_tmp_dir(execution_dir, consensus_dir).unwrap_err();

        assert!(err.to_string().contains("could not derive"));
    }

    #[tokio::test]
    async fn run_download_rejects_lone_consensus_url() {
        let args = DownloadArgs {
            execution_url: None,
            consensus_url: Some("http://x/cl".into()),
            chain: Some(Chain::Devnet),
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
            force_redownload: false,
            el_profile: ElProfile::Minimal,
        };
        let err = run_download(args).await.unwrap_err();
        assert!(err.to_string().contains("requires --execution-url"));
    }

    #[tokio::test]
    async fn run_download_rejects_lone_execution_url_without_chain() {
        let args = DownloadArgs {
            execution_url: Some("http://x/el".into()),
            consensus_url: None,
            chain: None,
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
            force_redownload: false,
            el_profile: ElProfile::Minimal,
        };
        let err = run_download(args).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "--execution-url requires --consensus-url; omit both to resolve a matched pair"
        );
    }

    #[tokio::test]
    async fn run_download_rejects_lone_execution_url_with_chain() {
        let args = DownloadArgs {
            execution_url: Some("http://x/el".into()),
            consensus_url: None,
            chain: Some(Chain::Devnet),
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
            force_redownload: false,
            el_profile: ElProfile::Minimal,
        };
        let err = run_download(args).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "--execution-url requires --consensus-url; omit both to resolve a matched pair"
        );
    }

    #[tokio::test]
    async fn run_download_errors_with_no_chain_and_no_urls() {
        let args = DownloadArgs {
            execution_url: None,
            consensus_url: None,
            chain: None,
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
            force_redownload: false,
            el_profile: ElProfile::Minimal,
        };
        let err = run_download(args).await.unwrap_err();
        assert!(err.to_string().contains("--chain is required"));
    }

    #[tokio::test]
    async fn run_download_manifest_requires_chain_even_with_urls() {
        // A manifest execution URL needs --chain even when both URLs are given —
        // arc-node-execution picks its chainspec from it. A single archive would
        // not, so this specifically exercises the manifest path.
        let args = DownloadArgs {
            execution_url: Some("http://x/manifest.json".into()),
            consensus_url: Some("http://x/cl".into()),
            chain: None,
            execution_path: Some("/tmp/el".into()),
            consensus_path: Some("/tmp/cl".into()),
            force_redownload: false,
            el_profile: ElProfile::Full,
        };
        let err = run_download(args).await.unwrap_err();
        assert!(err.to_string().contains("--chain is required"));
    }
}
