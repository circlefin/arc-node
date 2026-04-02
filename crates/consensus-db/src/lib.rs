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

#![allow(clippy::result_large_err)]

mod decoder;
mod encoder;

pub mod keys;
pub mod migrations;
pub mod repositories;
pub mod services;
pub mod versions;

mod store;
pub use store::{
    DbUpgrade, Store, StoreError, CERTIFICATES_TABLE, DECIDED_BLOCKS_TABLE, INVALID_PAYLOADS_TABLE,
    MISBEHAVIOR_EVIDENCE_TABLE, PENDING_PROPOSAL_PARTS_TABLE, PROPOSAL_MONITOR_DATA_TABLE,
    UNDECIDED_BLOCKS_TABLE,
};

mod metrics;
pub use metrics::DbMetrics;
pub mod invalid_payloads;
