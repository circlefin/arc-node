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

use std::error::Error;
use std::process::Command;

use vergen_git2::{BuildBuilder, Emitter, Git2Builder};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=ARC_IDEMPOTENT_BUILD");

    let idempotent = matches!(
        dotenvy::var("ARC_IDEMPOTENT_BUILD").as_deref(),
        Ok("1" | "true")
    );

    let mut emitter = Emitter::default();

    if idempotent {
        emitter.idempotent().quiet();
    }

    emitter
        .add_instructions(&BuildBuilder::all_build()?)?
        .add_instructions(&Git2Builder::all_git()?)?;

    // Use vergen to emit build metadata
    if let Err(e) = emitter.emit() {
        eprintln!("Failed to generate version metadata: {}", e);
        // Don't fail the build, just use defaults
    }

    // Emit custom version suffix based on git state
    emit_version_suffix();

    // Emit build profile
    emit_build_profile();

    // Emit short version string
    emit_short_version();

    // Emit long version strings
    emit_long_version(idempotent);

    if idempotent {
        // In idempotent builds, we don't care about Git changes
        return Ok(());
    }

    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned());

    // Let Cargo rerun when Git state changes
    if let Some(git_dir) = git_dir {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }

    let git_common_dir = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned());

    if let Some(git_common_dir) = git_common_dir {
        // For non-worktree repos, git-common-dir is the same as git-dir.
        // For worktrees, this points to the directory containing all repo metadata, including tags.
        println!("cargo:rerun-if-changed={git_common_dir}/refs/tags");
    }

    Ok(())
}

fn emit_version_suffix() {
    // Check if we're on a tag and if there are uncommitted changes
    let is_dirty = std::env::var("VERGEN_GIT_DIRTY")
        .map(|v| v == "true")
        .unwrap_or(false);

    let describe = std::env::var("VERGEN_GIT_DESCRIBE").unwrap_or_default();
    let sha = std::env::var("VERGEN_GIT_SHA").unwrap_or_default();

    // Check if we're exactly on a tag (describe output doesn't end with -g{sha})
    let on_exact_tag = if !sha.is_empty() && describe.len() >= 8 {
        !describe.ends_with(&format!("-g{}", &sha[..7]))
    } else {
        false
    };

    let suffix = if is_dirty || !on_exact_tag {
        "-dev"
    } else {
        ""
    };

    println!("cargo:rustc-env=ARC_VERSION_SUFFIX={}", suffix);
}

fn emit_build_profile() {
    let profile = get_build_profile();
    println!("cargo:rustc-env=ARC_BUILD_PROFILE={}", profile);
}

fn get_git_describe() -> String {
    std::env::var("VERGEN_GIT_DESCRIBE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["describe", "--tags", "--always", "--dirty"])
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn get_git_sha() -> String {
    std::env::var("VERGEN_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn extract_version_from_describe(git_describe: &str) -> &str {
    // Extract just the version tag (e.g., "v0.2.0-rc1" from "v0.2.0-rc1-74-g3ecc938-dirty")
    if let Some(pos) = git_describe.find("-g") {
        if let Some(commits_pos) = git_describe[..pos].rfind('-') {
            &git_describe[..commits_pos]
        } else {
            git_describe
        }
    } else {
        git_describe
    }
}

fn get_build_profile() -> String {
    std::env::var("PROFILE").unwrap_or_else(|_| {
        let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
        if out_dir.contains("/release/") {
            "release".to_string()
        } else if out_dir.contains("/debug/") {
            "debug".to_string()
        } else {
            "unknown".to_string()
        }
    })
}

fn emit_short_version() {
    let git_describe = get_git_describe();
    let version = extract_version_from_describe(&git_describe);
    let git_sha = get_git_sha();

    let short_sha = if git_sha.len() >= 8 {
        &git_sha[..8]
    } else {
        &git_sha
    };

    // Emit the short SHA as its own env var
    println!("cargo:rustc-env=VERGEN_GIT_SHA_SHORT={}", short_sha);

    // Short version: just tag and short hash (e.g., "v0.2.0-rc1 (3ecc9383)")
    let short_version = format!("{} ({})", version, short_sha);
    println!("cargo:rustc-env=ARC_SHORT_VERSION={}", short_version);
}

fn emit_long_version(idempotent: bool) {
    let git_describe = get_git_describe();
    let version = extract_version_from_describe(&git_describe);

    // Extract commits since tag (e.g., "74" from "v0.2.0-rc1-74-g3ecc938-dirty")
    let commits_since_tag = if let Some(pos) = git_describe.find("-g") {
        if let Some(commits_pos) = git_describe[..pos].rfind('-') {
            &git_describe[commits_pos + 1..pos]
        } else {
            "0"
        }
    } else {
        "0"
    };

    // Check if dirty
    let is_dirty = git_describe.ends_with("-dirty");

    let git_sha = get_git_sha();

    // Get build timestamp
    let build_timestamp = if idempotent {
        "1970-01-01T00:00:00+00:00".to_string()
    } else {
        chrono::Utc::now().to_rfc3339()
    };

    // Get platform
    let platform = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());

    let profile = get_build_profile();

    let features = std::env::var("CARGO_FEATURE_LIST").unwrap_or_else(|_| "".to_string());

    // Format: Multi-line version info
    let line0 = format!("Version: {}", version);
    let line1 = format!("Commit SHA: {}", git_sha);
    let line2 = format!("Commits Since Tag: {}", commits_since_tag);
    let line3 = format!("Dirty: {}", if is_dirty { "yes" } else { "no" });
    let line4 = format!("Build Timestamp: {}", build_timestamp);
    let line5 = format!(
        "Build Features: {}",
        if features.is_empty() {
            "default"
        } else {
            &features
        }
    );
    let line6 = format!("Build Profile: {}", profile);
    let line7 = format!("Platform: {}", platform);

    println!("cargo:rustc-env=ARC_LONG_VERSION_0={}", line0);
    println!("cargo:rustc-env=ARC_LONG_VERSION_1={}", line1);
    println!("cargo:rustc-env=ARC_LONG_VERSION_2={}", line2);
    println!("cargo:rustc-env=ARC_LONG_VERSION_3={}", line3);
    println!("cargo:rustc-env=ARC_LONG_VERSION_4={}", line4);
    println!("cargo:rustc-env=ARC_LONG_VERSION_5={}", line5);
    println!("cargo:rustc-env=ARC_LONG_VERSION_6={}", line6);
    println!("cargo:rustc-env=ARC_LONG_VERSION_7={}", line7);
}
