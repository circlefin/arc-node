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

use std::fmt;
use std::mem;

/// Current database schema version
/// Increment this when making schema changes that require migration (any of the following enums are changed)
pub const DB_SCHEMA_VERSION: SchemaVersion = SchemaVersion::V1;

/// Version type for various stored data
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SchemaVersion(u8);

impl SchemaVersion {
    pub const V0: Self = Self(0x00);
    pub const V1: Self = Self(0x01);

    pub const fn new(v: u8) -> Self {
        Self(v)
    }

    pub const fn as_u8(&self) -> u8 {
        self.0
    }

    pub const fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn previous(&self) -> Option<SchemaVersion> {
        self.0.checked_sub(1).map(Self::new)
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

impl redb::Value for SchemaVersion {
    type SelfType<'a>
        = SchemaVersion
    where
        Self: 'a;

    type AsBytes<'a>
        = [u8; 1]
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        Some(mem::size_of::<u8>())
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        SchemaVersion(data[0])
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        [value.as_u8()]
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("SchemaVersion")
    }
}

/// Assert at compile time that `Version` always has the same size as `u8` (ie. 1 byte)
const _: () = assert!(mem::size_of::<SchemaVersion>() == mem::size_of::<u8>());

/// Execution payload version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPayloadVersion {
    V3 = 0x03,
}

impl TryFrom<u8> for ExecutionPayloadVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x03 => Ok(Self::V3),
            _ => Err(value),
        }
    }
}

/// Proposal parts version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalPartsVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ProposalPartsVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Consensus block version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusBlockVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ConsensusBlockVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Commit certificate version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitCertificateVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for CommitCertificateVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Misbehavior evidence version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MisbehaviorEvidenceVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for MisbehaviorEvidenceVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Proposal monitor data version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION` when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalMonitorDataVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for ProposalMonitorDataVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}

/// Invalid payloads version
///
/// Do not forget to increment the `DB_SCHEMA_VERSION`
/// when changing this enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidPayloadsVersion {
    V1 = 0x01,
}

impl TryFrom<u8> for InvalidPayloadsVersion {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::V1),
            _ => Err(value),
        }
    }
}
