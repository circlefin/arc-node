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

pub type U64 = alloy_primitives::U64;
pub type U256 = alloy_primitives::U256;
pub type B256 = alloy_primitives::B256;

pub type BlockHash = alloy_primitives::BlockHash;
pub type BlockNumber = alloy_primitives::BlockNumber;
pub type BlockTimestamp = alloy_primitives::BlockTimestamp;
pub type Bloom = alloy_primitives::Bloom;
pub type Bytes = alloy_primitives::Bytes;

pub type Block = alloy_consensus::Block<TxEnvelope>;
pub type TxEnvelope = alloy_consensus::TxEnvelope;
