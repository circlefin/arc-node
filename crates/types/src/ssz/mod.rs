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

//! SSZ (Simple Serialize) encoding types.

/// Version 1 of the SSZ types.
pub mod v1;

// Re-export v1 types at the top level for convenience and backward compatibility
pub use v1::*;

// Also re-export submodules for those who want to use them explicitly
pub use v1::{nil_or_val, round};
pub mod vote {
    pub use super::v1::vote::*;
}
