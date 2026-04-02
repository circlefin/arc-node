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

use crate::ssz::SszSignature;
use crate::AlloyAddress;

/// SSZ encoding of a block.
pub type SszBlock<Payload> = (
    u64,                  // height
    Option<u32>,          // round
    Option<u32>,          // valid_round
    AlloyAddress,         // proposer
    bool,                 // is_valid
    Payload,              // execution_payload
    Option<SszSignature>, // signature
);
