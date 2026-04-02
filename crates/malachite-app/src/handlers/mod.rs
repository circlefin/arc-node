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

//! This module contains the handlers for messages received from the consensus engine.
//!
//! Each submodule corresponds to a variant of the `AppMsg` enum and
//! exposes a `handle` function to process the message.

pub mod consensus_ready;
pub mod decided;
pub mod finalized;
pub mod get_decided_values;
pub mod get_history_min_height;
pub mod get_value;
pub mod process_synced_value;
pub mod received_proposal_part;
pub mod restream_proposal;
pub mod started_round;
