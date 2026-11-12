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

#![allow(clippy::arithmetic_side_effects)]

mod arc_rpc_namespace;
mod base_fee;
mod beneficiary_blocklist;
mod block_hash_history;
mod block_production;
mod call_from_trace;
mod denylist;
mod eip161_empty_account_clearing;
mod eip7708_denylist;
mod eip7708_edge_cases;
mod eip7708_hardfork_transition;
mod eip7708_log_format;
mod eip7708_native_transfer;
mod eip7708_payload_validation;
mod eip7708_precompile;
mod eip7708_zero_address;
mod evict_unincludable_txs;
mod execution_cache_recreated_account;
mod gas_limit_validation;
mod hardfork_transition;
mod helpers;
mod invalid_tx_list;
mod native_transfer_balance;
mod p256_precompile;
mod pending_rpc;
mod pq_precompile;
mod raw_tx_params_shapes;
mod rpc_gas_cap_guard;
mod selfdestruct_beneficiary;
mod sparse_trie_payload_state_root;
mod static_rpc_gas_cap;
mod trace_byte_limit;
mod transaction;
mod withdrawals_payload_validation;
