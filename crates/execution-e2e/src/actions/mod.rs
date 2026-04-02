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

//! Actions for Arc e2e tests.
//!
//! Actions are composable building blocks for test scenarios.

mod assertions;
mod call_contract;
mod payload_utils;
mod produce_blocks;
mod produce_invalid_block;
mod send_transaction;

pub use assertions::{
    AssertBalance, AssertBlockNumber, AssertEthereumHardfork, AssertHardfork, AssertTxIncluded,
    AssertTxNotIncluded, TxStatus,
};
pub use call_contract::CallContract;
pub use payload_utils::{
    assert_valid_or_syncing, build_payload_for_next_block, set_payload_override_and_rehash,
    submit_payload,
};
pub use produce_blocks::ProduceBlocks;
pub use produce_invalid_block::ProduceInvalidBlock;
pub use send_transaction::SendTransaction;
