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

//! Custom error messages for CLI helper functions.
//! This low level implementation allows the developer to choose their own error handling library.

use std::path::PathBuf;

/// Error messages for commands
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Error creating parent directory
    #[error("Error creating parent directory: {}", .0.display())]
    ParentDir(PathBuf),

    /// Error opening file
    #[error("Error opening file: {}", .0.display())]
    OpenFile(PathBuf),

    /// Error writing file
    #[error("Error writing file: {}", .0.display())]
    WriteFile(PathBuf),

    /// Error loading file
    #[error("Error loading file: {}", .0.display())]
    LoadFile(PathBuf),

    /// Error converting to JSON
    #[error("Error converting to JSON: {0}")]
    ToJSON(String),

    /// Error parsing JSON
    #[error("Error parsing JSON: {0}")]
    FromJSON(String),

    /// Error deriving keys from BIP-39/BIP-32
    #[error("Error deriving test keys (BIP-39/BIP-32): {0}")]
    DeriveBip39(String),

    /// Error determining home directory path
    #[error("Error determining home directory path")]
    DirPath,

    /// Error joining threads
    #[error("Error joining threads")]
    Join,

    #[error("Error running spammer: {0}")]
    Spammer(eyre::Error),
}
