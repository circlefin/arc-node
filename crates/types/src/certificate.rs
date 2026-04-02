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

pub use malachitebft_core_types::{CommitCertificate, CommitSignature};

use crate::{Address, ArcContext};

/// The type of a commit certificate, which indicates the level
/// of information contained in the certificate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CommitCertificateType {
    /// The certificate type is not known.
    Unknown,

    /// The certificate contains only the minimal set of precommits.
    Minimal,

    /// The certificate contains additional precommits
    /// gathered during the finalization period.
    Extended,
}

impl CommitCertificateType {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Unknown => None,
            Self::Minimal => Some(false),
            Self::Extended => Some(true),
        }
    }

    pub fn is_extended(&self) -> bool {
        matches!(self, Self::Extended)
    }
}

impl From<Option<bool>> for CommitCertificateType {
    fn from(value: Option<bool>) -> Self {
        match value {
            None => Self::Unknown,
            Some(false) => Self::Minimal,
            Some(true) => Self::Extended,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCommitCertificate {
    pub certificate: CommitCertificate<ArcContext>,
    pub certificate_type: CommitCertificateType,
    pub proposer: Option<Address>,
}
