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

#![allow(async_fn_in_trait)]

mod certificates;
pub use certificates::CertificatesRepository;

mod decided_blocks;
pub use decided_blocks::DecidedBlocksRepository;

mod undecided_blocks;
pub use undecided_blocks::UndecidedBlocksRepository;

mod invalid_payloads;
pub use invalid_payloads::InvalidPayloadsRepository;

mod payloads;
pub use payloads::PayloadsRepository;

mod pending_proposals;
pub use pending_proposals::PendingProposalsRepository;

#[cfg(any(test, feature = "mock"))]
#[allow(unused_imports)]
pub mod mocks {
    pub use super::certificates::MockCertificatesRepository;
    pub use super::decided_blocks::MockDecidedBlocksRepository;
    pub use super::invalid_payloads::MockInvalidPayloadsRepository;
    pub use super::payloads::MockPayloadsRepository;
    pub use super::pending_proposals::MockPendingProposalsRepository;
    pub use super::undecided_blocks::MockUndecidedBlocksRepository;
}
