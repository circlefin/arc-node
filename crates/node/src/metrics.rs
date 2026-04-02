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

//! Metrics for Arc execution layer

use metrics::gauge;

/// Register and set version information metrics
pub fn register_version_info() {
    let version = arc_version::SHORT_VERSION;
    let git_commit = arc_version::GIT_COMMIT_HASH;

    // Register the version info metric with labels
    // This is a common pattern for exposing info-level data in Prometheus,
    // using a gauge with a constant value of 1.
    gauge!("arc_node_version_info",
        "version" => version,
        "git_commit" => git_commit
    )
    .set(1.0);
}
