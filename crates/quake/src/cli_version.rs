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

//! Version checks and compatibility shims for `arc-node-consensus` images.
//!
//! Two layers:
//!
//! * [`check_cli_version`] / [`supports_cli_flags`] decide whether a given
//!   target image is started with CLI flags (v0.5.0 and newer) or with a
//!   `config.toml` file (pre-v0.5.0). These helpers go away with the legacy
//!   `config.toml` code path once every scenario uses v0.5.0 or later.
//!
//! * [`apply_version_compat`] rewrites the CLI flag list produced by the
//!   current Quake binary so it matches the target image's `StartCmd`
//!   schema (flags added, removed, or renamed between the target release
//!   and the `StartCmd` definition compiled into this build). This layer
//!   is long-lived — it stays as long as Quake runs against older
//!   released images.

/// The minimum version that supports CLI flags instead of `config.toml`.
const MIN_CLI_FLAGS_VERSION: (u64, u64, u64) = (0, 5, 0);

//////////////////////////////////////////////////////////////////
// TODO: Remove once the network is fully migrated to use CLI flags. I.e when
// all scenarios use v0.5.0 or later.
//////////////////////////////////////////////////////////////////

/// Result of checking whether a CL image supports CLI flags.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CliVersionCheck {
    /// Version parsed and supports CLI flags (>= v0.5.0)
    SupportsCli,
    /// Version parsed and does NOT support CLI flags (< v0.5.0, needs config.toml)
    RequiresConfigToml,
    /// Version could not be parsed (e.g. git SHA); assumed to support CLI flags
    Assumed,
}

/// Detailed version check for safeguard validation.
pub(crate) fn check_cli_version(image_tag: Option<&str>) -> CliVersionCheck {
    let Some(tag) = image_tag else {
        return CliVersionCheck::Assumed;
    };
    let version_str = tag.rsplit(':').next().unwrap_or(tag);
    if version_str == "latest" {
        return CliVersionCheck::SupportsCli;
    }
    match parse_image_semver(image_tag) {
        Some(version) if version >= MIN_CLI_FLAGS_VERSION => CliVersionCheck::SupportsCli,
        Some(_) => CliVersionCheck::RequiresConfigToml,
        None => CliVersionCheck::Assumed,
    }
}

/// Check if an image tag version supports CLI flags.
///
/// Returns `false` only for versions definitively older than v0.5.0.
/// Returns `true` for `latest`, `None`, versions >= v0.5.0, and unparsable tags
/// (which are assumed to be v0.5.0+).
///
/// See [`check_cli_version`] for finer-grained distinction between confirmed
/// and assumed support.
pub(crate) fn supports_cli_flags(image_tag: Option<&str>) -> bool {
    check_cli_version(image_tag) != CliVersionCheck::RequiresConfigToml
}

//////////////////////////////////////////////////////////////////
// END OF TODO: Remove once the network is fully migrated to use CLI flags. I.e
// when all scenarios use v0.5.0 or later.
//////////////////////////////////////////////////////////////////

/// Released CL versions referenced as boundaries by [`apply_version_compat`].
const V0_5_0: (u64, u64, u64) = (0, 5, 0);
const V0_6_0: (u64, u64, u64) = (0, 6, 0);

/// Extract a `(major, minor, patch)` tuple from an image tag.
///
/// Returns `Some` only for explicit parseable versions such as `v0.6.0`,
/// `arc_consensus:v0.5.1-rc1`, or `0.7.0`. Returns `None` for missing tags,
/// the `"latest"` tag, or tags that do not fit the `MAJOR.MINOR.PATCH[-...]`
/// pattern. Callers decide how to interpret `None` — [`check_cli_version`]
/// distinguishes `latest` from unparsable, while [`apply_version_compat`]
/// treats every `None` uniformly as "assume the target supports every flag".
fn parse_image_semver(image_tag: Option<&str>) -> Option<(u64, u64, u64)> {
    let tag = image_tag?;
    let version_str = tag.rsplit(':').next().unwrap_or(tag);
    if version_str == "latest" {
        return None;
    }
    let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
    let parts: Vec<&str> = version_str.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts[1].parse::<u64>().ok()?;
    let patch_str = parts[2].split('-').next().unwrap_or(parts[2]);
    let patch = patch_str.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// Rewrite the CLI flag list produced by this Quake build so it matches the
/// `StartCmd` schema of the target `arc-node-consensus` image, and return
/// the rewritten list.
///
/// The input is what `StartCmd::to_cli_flags()` emits for the current Quake
/// binary — it reflects the `StartCmd` definition as of the commit Quake was
/// built from, which may have gained, renamed, or removed flags relative to
/// older released images. Each `if` block below handles one target version's
/// deviation from the current build; blocks are ordered oldest-to-newest.
/// `None`, `"latest"`, and unparsable tags short-circuit the whole pass and
/// return the input unchanged.
///
/// When a change to `StartCmd`'s CLI schema lands and any pinned image
/// in scenario rotation — the `image_cl` / `image_cl_upgrade` fields in
/// every TOML under `crates/quake/scenarios/` — would fail to parse the
/// resulting flag list, add a compat block in the same change.
/// Concretely:
///
/// * Adding a flag: drop it for every pinned tag released before the
///   flag existed.
/// * Renaming a flag: rewrite the new name back to the old one for every
///   pinned tag released before the rename.
/// * Removing a flag while keeping the `StartCmd` field as a deprecated
///   stub: drop the flag for every pinned tag released after the
///   removal.
///
/// When all pinned tags are newer than the change (or every scenario
/// uses `"latest"`), no block is needed. Anchor each version bound on
/// the *last released tag that demonstrably lacks the change*, so the
/// block stays correct for every later release without another edit.
/// Any pure function of the flag list and the target version belongs
/// here — drop, rewrite, collapse two into one, split one into two, etc.
///
/// A `StartCmd` field deleted outright with no deprecated stub cannot
/// be expressed here: `to_cli_flags()` has nothing to emit, so no
/// post-processing can synthesize the flag. Keep such fields as
/// `#[arg(hide = true)]` in the CL crate until every pinned image stops
/// needing them.
pub(crate) fn apply_version_compat(mut flags: Vec<String>, image_tag: Option<&str>) -> Vec<String> {
    let Some(version) = parse_image_semver(image_tag) else {
        return flags;
    };

    // Apply V0_5_0 compat.
    if version <= V0_5_0 {
        const FLAGS_ADDED_AFTER_V0_5_0: &[&str] = &[
            "--log-level",
            "--log-format",
            "--p2p.persistent-peers-only",
            "--gossipsub.explicit-peering",
            "--gossipsub.mesh-prioritization",
            "--gossipsub.load",
            "--execution-persistence-backpressure",
            "--execution-persistence-backpressure-threshold",
            "--execution-ws-endpoint",
            "--full",
            "--minimal",
            "--pprof.heap-prof",
        ];
        flags.retain(|f| {
            !FLAGS_ADDED_AFTER_V0_5_0
                .iter()
                .any(|added| flag_matches(f, added))
        });

        // Rewrite renamed flags, preserving any `=value` suffix.
        for f in &mut flags {
            if let Some(rest) = f.strip_prefix("--prune.certificates.distance") {
                if rest.starts_with('=') {
                    *f = format!("--pruning.block-interval{rest}");
                }
            } else if let Some(rest) = f.strip_prefix("--prune.certificates.before") {
                if rest.starts_with('=') {
                    *f = format!("--pruning.min-height{rest}");
                }
            }
        }
    }

    // Apply V0_6_0 compat.
    if version <= V0_6_0 {
        flags.retain(|f| !flag_matches(f, "--validator"));
    }

    flags
}

/// `to_cli_flags` emits either `"--foo"` (booleans) or `"--foo=<value>"`
/// (values, including comma-joined lists like `"--foo=a,b,c"`). Both forms
/// match a bare `"--foo"` entry.
fn flag_matches(emitted: &str, flag_name: &str) -> bool {
    emitted == flag_name
        || emitted
            .strip_prefix(flag_name)
            .is_some_and(|rest| rest.starts_with('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_cli_flags_returns_true_for_latest() {
        assert!(supports_cli_flags(Some("arc_consensus:latest")));
        assert!(supports_cli_flags(Some("latest")));
    }

    #[test]
    fn supports_cli_flags_returns_true_for_new_versions() {
        assert!(supports_cli_flags(Some("arc_consensus:v0.5.0")));
        assert!(supports_cli_flags(Some("arc_consensus:v0.6.0")));
        assert!(supports_cli_flags(Some("arc_consensus:v1.0.0")));
        assert!(supports_cli_flags(Some("v0.5.0")));
        assert!(supports_cli_flags(Some("0.5.0")));
    }

    #[test]
    fn supports_cli_flags_returns_false_for_old_versions() {
        assert!(!supports_cli_flags(Some("arc_consensus:v0.4.0")));
        assert!(!supports_cli_flags(Some("arc_consensus:v0.4.1")));
        assert!(!supports_cli_flags(Some("arc_consensus:v0.3.0")));
        assert!(!supports_cli_flags(Some("v0.4.0")));
        assert!(!supports_cli_flags(Some("0.4.0")));
    }

    #[test]
    fn supports_cli_flags_returns_true_for_none() {
        assert!(supports_cli_flags(None));
    }

    #[test]
    fn supports_cli_flags_handles_prerelease_versions() {
        assert!(supports_cli_flags(Some("v0.5.0-rc1")));
        assert!(supports_cli_flags(Some("v0.5.0-beta")));
        assert!(!supports_cli_flags(Some("v0.4.0-rc1")));
    }

    /// `check_cli_version` exposes a three-state result. `supports_cli_flags`
    /// only exercises the `RequiresConfigToml` boundary, so the distinction
    /// between confirmed-`SupportsCli` and unparseable-`Assumed` is only
    /// covered implicitly. This test pins the three states down so a
    /// future refactor cannot quietly collapse `Assumed` into `SupportsCli`
    /// (or vice versa) — they are documented as distinct so callers can
    /// log or branch on unparseable tags if needed.
    #[test]
    fn check_cli_version_distinguishes_confirmed_from_assumed() {
        // Confirmed: parseable version >= v0.5.0 -> SupportsCli.
        assert_eq!(
            check_cli_version(Some("v0.5.0")),
            CliVersionCheck::SupportsCli
        );
        assert_eq!(
            check_cli_version(Some("arc_consensus:v0.6.0")),
            CliVersionCheck::SupportsCli
        );

        // Confirmed: parseable version < v0.5.0 -> RequiresConfigToml.
        assert_eq!(
            check_cli_version(Some("v0.4.0")),
            CliVersionCheck::RequiresConfigToml
        );
        assert_eq!(
            check_cli_version(Some("arc_consensus:v0.3.0")),
            CliVersionCheck::RequiresConfigToml
        );

        // "latest" is an explicit "we know this is fine" signal, not an
        // assumption. It must report SupportsCli, not Assumed.
        assert_eq!(
            check_cli_version(Some("latest")),
            CliVersionCheck::SupportsCli
        );
        assert_eq!(
            check_cli_version(Some("arc_consensus:latest")),
            CliVersionCheck::SupportsCli
        );

        // Unparseable tags (git SHAs, branches, "main", etc.) are
        // assumptions, not confirmations. This is the only situation
        // that produces Assumed besides missing tags.
        assert_eq!(check_cli_version(Some("abc123")), CliVersionCheck::Assumed);
        assert_eq!(check_cli_version(Some("main")), CliVersionCheck::Assumed);

        // Missing tag is the original Assumed case.
        assert_eq!(check_cli_version(None), CliVersionCheck::Assumed);
    }

    /// Run `apply_version_compat` over `input` for the given `image_tag` and
    /// assert the rewritten list equals `expected`.
    #[track_caller]
    fn assert_compat(image_tag: &str, input: &[&str], expected: &[&str]) {
        let input: Vec<String> = input.iter().map(|s| s.to_string()).collect();
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        let result = apply_version_compat(input, Some(image_tag));
        assert_eq!(result, expected, "compat mismatch for tag {image_tag}");
    }

    fn compat_flags(image_tag: Option<&str>) -> Vec<String> {
        let flags = vec![
            "--moniker=val-1".to_string(),
            "--validator".to_string(),
            "--suggested-fee-recipient=0x1".to_string(),
        ];
        apply_version_compat(flags, image_tag)
    }

    #[test]
    fn apply_version_compat_passes_flags_through_for_missing_latest_and_unparsable_tags() {
        for tag in [
            None,
            Some("arc_consensus:latest"),
            Some("latest"),
            Some("arc_consensus:abc123"),
            Some("main"),
        ] {
            let result = compat_flags(tag);
            assert!(
                result.iter().any(|f| f == "--validator"),
                "--validator should survive compat pass for tag {tag:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn apply_version_compat_drops_validator_for_images_at_or_below_v0_6_0() {
        for tag in [
            "arc_consensus:v0.6.0",
            "arc_consensus:v0.5.9",
            "arc_consensus:v0.4.0",
            "v0.6.0",
            "0.6.0",
            "v0.6.0-rc1",
        ] {
            let result = compat_flags(Some(tag));
            assert!(
                !result.iter().any(|f| f == "--validator"),
                "--validator should be dropped for tag {tag:?}, got {result:?}"
            );
            assert!(result.iter().any(|f| f == "--moniker=val-1"));
            assert!(result.iter().any(|f| f == "--suggested-fee-recipient=0x1"));
        }
    }

    #[test]
    fn apply_version_compat_keeps_validator_for_images_strictly_newer_than_v0_6_0() {
        for tag in [
            "arc_consensus:v0.6.1",
            "arc_consensus:v0.6.2",
            "arc_consensus:v0.7.0",
            "arc_consensus:v1.0.0",
            "v0.6.1",
            "0.6.1",
            "v0.6.1-rc1",
            "v0.7.0-beta",
        ] {
            let result = compat_flags(Some(tag));
            assert!(
                result.iter().any(|f| f == "--validator"),
                "--validator should survive compat pass for tag {tag:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn flag_matches_handles_boolean_and_value_forms() {
        assert!(flag_matches("--validator", "--validator"));
        assert!(flag_matches("--moniker=val-1", "--moniker"));
        assert!(flag_matches(
            "--p2p.persistent-peers=a,b,c",
            "--p2p.persistent-peers"
        ));
        assert!(!flag_matches("--validator-set", "--validator"));
        assert!(!flag_matches("--moniker=validator", "--validator"));
        assert!(!flag_matches("--validator.something", "--validator"));
    }

    #[test]
    fn apply_version_compat_drops_v0_5_era_added_flags_for_v0_5_0() {
        assert_compat(
            "arc_consensus:v0.5.0",
            &[
                "--moniker=val-1",
                "--log-level=info",
                "--log-format=json",
                "--p2p.persistent-peers-only",
                "--gossipsub.explicit-peering",
                "--gossipsub.mesh-prioritization",
                "--gossipsub.load=high",
                "--execution-persistence-backpressure",
                "--execution-persistence-backpressure-threshold=5",
                "--execution-ws-endpoint=ws://el:8546",
                "--full",
                "--minimal",
                "--pprof.heap-prof",
            ],
            &["--moniker=val-1"],
        );
    }

    #[test]
    fn apply_version_compat_keeps_v0_5_era_added_flags_for_v0_5_1_and_later() {
        for tag in ["v0.5.1", "v0.6.0", "v0.7.0", "v0.5.1-rc1"] {
            assert_compat(
                tag,
                &["--log-level=info", "--gossipsub.load=high", "--full"],
                &["--log-level=info", "--gossipsub.load=high", "--full"],
            );
        }
    }

    #[test]
    fn apply_version_compat_renames_pruning_flags_for_v0_5_0() {
        assert_compat(
            "arc_consensus:v0.5.0",
            &[
                "--moniker=val-1",
                "--prune.certificates.distance=237600",
                "--prune.certificates.before=100",
            ],
            &[
                "--moniker=val-1",
                "--pruning.block-interval=237600",
                "--pruning.min-height=100",
            ],
        );
    }

    #[test]
    fn apply_version_compat_keeps_new_pruning_names_for_v0_5_1_and_later() {
        for tag in ["v0.5.1", "v0.6.0", "v0.7.0"] {
            assert_compat(
                tag,
                &[
                    "--prune.certificates.distance=237600",
                    "--prune.certificates.before=100",
                ],
                &[
                    "--prune.certificates.distance=237600",
                    "--prune.certificates.before=100",
                ],
            );
        }
    }
}
