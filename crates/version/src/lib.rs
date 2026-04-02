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

//! Version information for Arc
//!
//! This crate provides version information captured at build time from git,
//! cargo, and build metadata using the vergen pattern similar to Reth.
//!
//! # Examples
//!
//! ```
//! use arc_version::{SHORT_VERSION, LONG_VERSION};
//!
//! println!("Version: {}", SHORT_VERSION);
//! println!("Details:\n{}", LONG_VERSION);
//! ```

/// Short version string for CLI display
/// Format: "v0.2.0-rc1 (3ecc9383)"
pub const SHORT_VERSION: &str = env!("ARC_SHORT_VERSION");

/// Long version string with detailed build information
/// Includes: version, commit SHA, commits since tag, dirty flag, timestamp, features, profile, platform
pub const LONG_VERSION: &str = concat!(
    env!("ARC_LONG_VERSION_0"),
    "\n",
    env!("ARC_LONG_VERSION_1"),
    "\n",
    env!("ARC_LONG_VERSION_2"),
    "\n",
    env!("ARC_LONG_VERSION_3"),
    "\n",
    env!("ARC_LONG_VERSION_4"),
    "\n",
    env!("ARC_LONG_VERSION_5"),
    "\n",
    env!("ARC_LONG_VERSION_6"),
    "\n",
    env!("ARC_LONG_VERSION_7")
);

// Legacy constants for backward compatibility
/// Git version (tag or short commit hash)
/// Deprecated: Use SHORT_VERSION or version_metadata() instead
pub const GIT_VERSION: &str = env!("VERGEN_GIT_DESCRIBE");

/// Full git commit hash (40 characters)
/// Deprecated: Use version_metadata().git_sha instead
pub const GIT_COMMIT_HASH: &str = env!("VERGEN_GIT_SHA");

/// Short git commit hash (8 characters)
/// Deprecated: Use version_metadata().git_sha_short instead
pub const GIT_SHORT_HASH: &str = env!("VERGEN_GIT_SHA_SHORT");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_constants_exist() {
        assert_ne!(GIT_COMMIT_HASH, "");
        assert_ne!(GIT_SHORT_HASH, "");
        assert_ne!(SHORT_VERSION, "");
        assert_ne!(LONG_VERSION, "");
    }

    #[test]
    fn test_git_commit_hash_length() {
        // Full commit hash should be 40 characters (SHA-1)
        assert_eq!(
            GIT_COMMIT_HASH.len(),
            40,
            "Full commit hash should be 40 characters"
        );
    }

    #[test]
    fn test_git_short_hash_length() {
        // Short hash should be 8 characters (updated from 7 to match Reth)
        println!(
            "GIT_SHORT_HASH: '{}' (len: {})",
            GIT_SHORT_HASH,
            GIT_SHORT_HASH.len()
        );
        println!(
            "GIT_COMMIT_HASH: '{}' (len: {})",
            GIT_COMMIT_HASH,
            GIT_COMMIT_HASH.len()
        );
        assert_eq!(GIT_SHORT_HASH.len(), 8, "Short hash should be 8 characters");
    }

    #[test]
    fn test_short_version_format() {
        // Should contain short hash in parentheses
        assert!(SHORT_VERSION.contains(GIT_SHORT_HASH));
        assert!(SHORT_VERSION.contains('('));
        assert!(SHORT_VERSION.contains(')'));
    }

    #[test]
    fn test_long_version_multiline() {
        // Long version should have multiple lines with all expected fields
        assert!(LONG_VERSION.contains('\n'));
        assert!(LONG_VERSION.contains("Version:"));
        assert!(LONG_VERSION.contains("Commit SHA:"));
        assert!(LONG_VERSION.contains("Commits Since Tag:"));
        assert!(LONG_VERSION.contains("Dirty:"));
        assert!(LONG_VERSION.contains("Build Timestamp:"));
        assert!(LONG_VERSION.contains("Build Features:"));
        assert!(LONG_VERSION.contains("Build Profile:"));
        assert!(LONG_VERSION.contains("Platform:"));
    }
}
