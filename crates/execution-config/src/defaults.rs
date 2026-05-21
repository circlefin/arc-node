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

//! Default configuration for Arc Network node.
//!
//! This module provides default values for various node components including
//! snapshot download URLs for quick node bootstrapping.

use reth_cli_commands::download::DownloadDefaults;
use std::borrow::Cow;

// FIXME: Update this to the actual snapshot URL.
/// Default snapshot URL for Arc Network testnet (chain ID 5042002).
pub(crate) const DEFAULT_DOWNLOAD_URL: &str = "https://snapshots.arc.network/5042002";

/// Initialize download URL defaults for snapshot-based node bootstrapping.
///
/// This registers snapshot URLs for Arc Network chains (testnet and devnet)
/// which can be used with the `arc-node-execution download` command.
fn init_download_urls() {
    let download_defaults = DownloadDefaults {
        available_snapshots: vec![
            // FIXME: Update this to the actual snapshot URL.
            Cow::Borrowed("https://snapshots.arc.network/5042002 (testnet)"),
            Cow::Borrowed("https://snapshots.arc.network/5042001 (devnet)"),
        ],
        default_base_url: Cow::Borrowed(DEFAULT_DOWNLOAD_URL),
        default_chain_aware_base_url: None,
        long_help: None,
    };

    download_defaults
        .try_init()
        .expect("failed to initialize download URLs");
}

/// Initialize all Arc Network node defaults.
///
/// This function must be called before parsing CLI arguments to ensure
/// defaults are registered with Reth's command infrastructure.
///
/// Currently initializes:
/// - Download URLs for snapshot-based bootstrapping
pub fn init_defaults() {
    init_download_urls();
}
