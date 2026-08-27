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

//! Manifest snapshot restore.
//!
//! A manifest restore delegates the execution layer to the
//! `arc-node-execution download` command (reth's `DownloadCommand`), while the
//! consensus layer keeps arc-snapshots' own archive download.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::Path,
    process::{Command, Stdio},
};

use eyre::Result;

use crate::download::{url_identity, Chain};

/// Execution-layer component set for a manifest restore.
///
/// Maps to `arc-node-execution download`'s `--minimal` / `--full` / `--archive`
/// presets. Has no effect for a caller-supplied single archive, whose contents
/// determine the data restored.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum ElProfile {
    /// State + all headers + small unwind buffer.
    #[value(name = "minimal")]
    Minimal,
    /// Full transactions, receipts, and changesets.
    #[value(name = "full")]
    Full,
    /// Every component, incl. transaction senders and rocksdb indices.
    #[value(name = "archive")]
    Archive,
}

impl ElProfile {
    /// The `arc-node-execution download` preset flag for this profile.
    fn flag(self) -> &'static str {
        match self {
            Self::Minimal => "--minimal",
            Self::Full => "--full",
            Self::Archive => "--archive",
        }
    }
}

impl std::fmt::Display for ElProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Minimal => write!(f, "minimal"),
            Self::Full => write!(f, "full"),
            Self::Archive => write!(f, "archive"),
        }
    }
}

/// The `.snapshot-url` marker value for a completed manifest restore.
///
/// The profile is part of the identity, not just the URL: the same manifest
/// restored as `minimal` and as `archive` produce different datadirs, so a
/// profile change must not read as an up-to-date restore.
///
/// The URL contributes only its [`url_identity`], so a re-signed pre-signed URL
/// does not read as a different snapshot. Composing after stripping also keeps
/// the profile suffix out of reach of a second normalization pass.
pub fn manifest_marker(manifest_url: &str, profile: ElProfile) -> String {
    format!("{} el-profile={profile}", url_identity(manifest_url))
}

/// Inputs for an execution-layer download delegated to
/// `arc-node-execution download`.
pub struct ExecutionDownload<'a> {
    /// Target chain; supplies the `--chain` value via [`Chain::arc_chain_arg`].
    pub chain: Chain,
    /// Component preset to fetch.
    pub profile: ElProfile,
    /// Target reth datadir, passed verbatim as `--datadir`.
    pub datadir: &'a Path,
    /// Manifest URL, passed as `--manifest-url`. Content-based dispatch only
    /// reaches this path when a manifest URL is known, so it is always present.
    pub manifest_url: &'a str,
}

/// The flag that carries the manifest URL to `arc-node-execution download`.
///
/// Shared between the argv and the pre-flight check, so the probe verifies the
/// flag the restore actually passes.
///
/// reth's `DownloadCommand` is what names it. Should that name change, every
/// manifest restore fails the probe with "needs a newer execution binary", which is
/// the wrong explanation but at least stops before deleting anything.
const MANIFEST_URL_FLAG: &str = "--manifest-url";

/// The file reth leaves behind once a manifest download has finished.
///
/// `reth.toml` is reth's config file, written by `write_config`. That is the
/// first thing that reth does after every selected snapshot-manifest archive has
/// been downloaded and verified, so finding the file means the download got to the
/// end.
///
/// That holds only because [`clear_datadir`] empties the datadir immediately
/// before the download runs. A node that has been running already has a
/// `reth.toml` of its own, and reth's `write_config` leaves an existing file alone
/// instead of replacing it. So without the wipe, that old file would still be
/// there after a download that stopped halfway, and the check would read it as a
/// success. Anything that stops wiping the datadir — making the download
/// resumable, say — needs a different completion signal.
const COMPLETION_FILE: &str = "reth.toml";

/// Empties the execution datadir so a manifest restore starts from nothing.
///
/// `arc-node-execution download` writes only the files its manifest lists, and
/// straight into the datadir — there is no staging step. Anything already there
/// survives and mixes with the new data, so it has to go first, which also means
/// a restore that fails leaves the datadir empty.
pub fn clear_datadir(datadir: &Path) -> Result<()> {
    crate::download::remove_restore_dir(datadir)
}

/// Build the argument vector for `arc-node-execution download` from `opts`.
///
/// `--force` is intentionally absent: reth's download has no such flag, so the
/// manifest path achieves a clean restore by wiping the datadir before
/// invoking.
pub fn build_execution_argv(opts: &ExecutionDownload) -> Vec<OsString> {
    vec![
        "download".into(),
        "--chain".into(),
        opts.chain.arc_chain_arg().into(),
        "--datadir".into(),
        opts.datadir.into(),
        opts.profile.flag().into(),
        MANIFEST_URL_FLAG.into(),
        opts.manifest_url.into(),
    ]
}

/// Runs the `arc-node-execution download` command.
///
/// This is a trait so tests can swap in a fake that records how it was
/// called instead of actually launching the binary.
pub trait ExecutionDownloader {
    /// Fail unless `binary` can carry out a manifest download.
    ///
    /// Called before a restore deletes anything. The execution datadir is
    /// removed before reth writes into it, so this has to establish more than
    /// "the binary starts": a build predating reth's `download` command, or
    /// predating its manifest support, would pass that and still fail after the
    /// data was gone.
    fn ensure_available(&self, binary: &OsStr) -> Result<()>;

    /// Run `binary` with the given arguments. Returns an error if the binary
    /// can't be started or exits with a failure code.
    fn run(&self, binary: &OsStr, argv: &[OsString]) -> Result<()>;
}

/// The [`ExecutionDownloader`] used in real runs. It launches the binary as a
/// child process, passes the child's output straight through to the terminal
/// (so you see the download progress), and waits for it to finish.
pub struct CommandDownloader;

impl ExecutionDownloader for CommandDownloader {
    fn ensure_available(&self, binary: &OsStr) -> Result<()> {
        let name = binary.to_string_lossy();
        // `download --help` rather than `--version`: it asks the binary whether
        // it has the subcommand this restore needs. clap prints every long flag
        // in its help output, so the presence of the flag we pass is checkable
        // from the same invocation.
        let output = Command::new(binary)
            .args(["download", "--help"])
            .stderr(Stdio::null())
            .output()
            .map_err(|e| spawn_error(binary, e))?;

        if !output.status.success() {
            eyre::bail!(
                "`{name} download` is unavailable (exited with {}); a manifest \
                 restore needs an execution binary that carries reth's download command",
                output.status
            );
        }
        if !String::from_utf8_lossy(&output.stdout).contains(MANIFEST_URL_FLAG) {
            eyre::bail!(
                "`{name} download` does not accept {MANIFEST_URL_FLAG}; a manifest \
                 restore needs a newer execution binary"
            );
        }
        Ok(())
    }

    fn run(&self, binary: &OsStr, argv: &[OsString]) -> Result<()> {
        let status = Command::new(binary)
            .args(argv)
            .status()
            .map_err(|e| spawn_error(binary, e))?;
        if !status.success() {
            let name = binary.to_string_lossy();
            eyre::bail!("`{name} download` exited with {status}");
        }
        Ok(())
    }
}

/// Turns a failure to launch `binary` into a user-facing error.
fn spawn_error(binary: &OsStr, e: io::Error) -> eyre::Report {
    let binary = binary.to_string_lossy();
    if e.kind() == io::ErrorKind::NotFound {
        eyre::eyre!(
            "execution binary `{binary}` not found on PATH; \
             set ARC_EXECUTION_BINARY to its path"
        )
    } else {
        eyre::eyre!("failed to run `{binary}`: {e}")
    }
}

/// Build the download argv from `opts` and run it via `downloader`.
pub fn run_execution_download(
    downloader: &dyn ExecutionDownloader,
    binary: &OsStr,
    opts: &ExecutionDownload,
) -> Result<()> {
    downloader.run(binary, &build_execution_argv(opts))?;

    if !opts.datadir.join(COMPLETION_FILE).exists() {
        eyre::bail!(
            "`{} download` exited successfully but left no {COMPLETION_FILE} in {}, \
             so the download did not finish and the datadir holds only part of the \
             snapshot",
            binary.to_string_lossy(),
            opts.datadir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// Records each invocation instead of spawning a process.
    struct RecordingDownloader {
        calls: RefCell<Vec<(OsString, Vec<OsString>)>>,
    }

    impl RecordingDownloader {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ExecutionDownloader for RecordingDownloader {
        fn ensure_available(&self, _binary: &OsStr) -> Result<()> {
            Ok(())
        }

        fn run(&self, binary: &OsStr, argv: &[OsString]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push((binary.to_os_string(), argv.to_vec()));
            let datadir = argv
                .iter()
                .position(|a| a == "--datadir")
                .and_then(|i| argv.get(i.saturating_add(1)))
                .map(Path::new)
                .expect("argv always carries --datadir");
            std::fs::create_dir_all(datadir)?;
            std::fs::write(datadir.join(COMPLETION_FILE), b"[stages]")?;
            Ok(())
        }
    }

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|&s| OsString::from(s)).collect()
    }

    #[test]
    fn run_execution_download_passes_expected_argv() {
        let recorder = RecordingDownloader::new();
        let dir = tempfile::tempdir().unwrap();
        let datadir = dir.path().join("execution");
        let opts = ExecutionDownload {
            chain: Chain::Devnet,
            profile: ElProfile::Full,
            datadir: &datadir,
            manifest_url: "https://x.example/m.json",
        };
        run_execution_download(&recorder, OsStr::new("arc-node-execution"), &opts).unwrap();

        let calls = recorder.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, OsStr::new("arc-node-execution"));
        assert_eq!(
            calls[0].1,
            argv(&[
                "download",
                "--chain",
                "arc-devnet",
                "--datadir",
                datadir.to_str().unwrap(),
                "--full",
                "--manifest-url",
                "https://x.example/m.json",
            ])
        );
    }

    #[test]
    fn run_execution_download_rejects_a_zero_exit_that_wrote_no_config() {
        // reth's download runs under a ctrl-c runner that returns success when the
        // process is signalled, so a zero exit can mean "abandoned partway". The
        // config file it writes at the end is what separates the two.
        struct SilentSuccess;
        impl ExecutionDownloader for SilentSuccess {
            fn ensure_available(&self, _binary: &OsStr) -> Result<()> {
                Ok(())
            }
            fn run(&self, _binary: &OsStr, _argv: &[OsString]) -> Result<()> {
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let datadir = dir.path().join("execution");
        let opts = ExecutionDownload {
            chain: Chain::Devnet,
            profile: ElProfile::Full,
            datadir: &datadir,
            manifest_url: "https://x.example/m.json",
        };

        let err = run_execution_download(&SilentSuccess, OsStr::new("arc-node-execution"), &opts)
            .unwrap_err();
        assert!(
            err.to_string().contains(COMPLETION_FILE),
            "unexpected: {err}"
        );
    }

    #[test]
    fn command_downloader_errors_on_nonzero_exit() {
        // `false` is a POSIX utility that always exits non-zero.
        let err = CommandDownloader.run(OsStr::new("false"), &[]).unwrap_err();
        assert!(err.to_string().contains("exited with"), "unexpected: {err}");
    }

    #[test]
    fn ensure_available_rejects_a_binary_without_the_download_command() {
        // `false` stands in for a build predating reth's download command: it
        // launches, and rejects the subcommand.
        let err = CommandDownloader
            .ensure_available(OsStr::new("false"))
            .unwrap_err();
        assert!(
            err.to_string().contains("download` is unavailable"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn ensure_available_rejects_a_binary_without_manifest_support() {
        // `true` stands in for a build whose download command predates manifests:
        // the subcommand is accepted, but the flag this restore passes is absent
        // from its help.
        let err = CommandDownloader
            .ensure_available(OsStr::new("true"))
            .unwrap_err();
        assert!(
            err.to_string().contains(MANIFEST_URL_FLAG),
            "unexpected: {err}"
        );
    }

    #[test]
    fn ensure_available_reports_a_missing_binary() {
        // The message must name the override, since this check is what stops a
        // forced restore from deleting data it cannot replace.
        let err = CommandDownloader
            .ensure_available(OsStr::new("arc-node-execution-does-not-exist"))
            .unwrap_err();
        assert!(
            err.to_string().contains("not found on PATH"),
            "unexpected: {err}"
        );
        assert!(
            err.to_string().contains("ARC_EXECUTION_BINARY"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn manifest_marker_records_the_url_and_the_profile() {
        let url = "https://x.example/testnet/manifest.json";
        assert_eq!(
            manifest_marker(url, ElProfile::Minimal),
            "https://x.example/testnet/manifest.json el-profile=minimal"
        );
    }

    #[test]
    fn flag_matches_the_clap_value_name() {
        // `flag` restates the #[value(name = ...)] strings as `--<name>`, and the
        // two have to agree: the name is what an operator passes to
        // --el-profile, the flag is what reaches arc-node-execution. Iterating
        // value_variants() covers a new profile automatically.
        use clap::ValueEnum;

        for profile in ElProfile::value_variants() {
            let value = profile.to_possible_value().unwrap();
            assert_eq!(profile.flag(), format!("--{}", value.get_name()));
        }
    }

    #[test]
    fn manifest_marker_ignores_a_pre_signed_signature() {
        // Otherwise a re-signed URL reads as a different snapshot and the whole
        // execution layer is fetched again.
        let clean = "https://x.example/testnet/manifest.json";
        let signed = "https://x.example/testnet/manifest.json?X-Amz-Signature=deadbeef";
        assert_eq!(
            manifest_marker(signed, ElProfile::Full),
            manifest_marker(clean, ElProfile::Full)
        );
        assert!(!manifest_marker(signed, ElProfile::Full).contains("Signature"));
    }

    #[test]
    fn manifest_marker_differs_per_profile() {
        // A profile change must not read as an up-to-date restore, so the same
        // manifest URL has to produce a different marker per profile.
        let url = "https://x.example/testnet/manifest.json";
        let markers = [
            manifest_marker(url, ElProfile::Minimal),
            manifest_marker(url, ElProfile::Full),
            manifest_marker(url, ElProfile::Archive),
        ];
        for (i, a) in markers.iter().enumerate() {
            for b in markers.iter().skip(i.saturating_add(1)) {
                assert_ne!(a, b);
            }
        }
    }
}
