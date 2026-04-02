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

//! Subcall registry and continuation storage for `ArcEvm`.
//!
//! The [`SubcallPrecompile`] trait and related types are defined in `arc-precompiles`.
//! This module provides the registry that maps precompile addresses to implementations,
//! and the continuation type stored on `ArcEvm` between `init_subcall` and `complete_subcall`.

use alloy_primitives::Address;
use arc_precompiles::subcall::{SubcallContinuationData, SubcallPrecompile};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// State stored between `init_subcall` and `complete_subcall`, keyed by the precompile call's depth.
///
/// Contains a trait object (`dyn SubcallPrecompile`) which doesn't implement `Debug`,
/// so we provide a manual implementation.
pub struct SubcallContinuation {
    /// The subcall precompile implementation that handles `complete_subcall`.
    pub(crate) precompile: Arc<dyn SubcallPrecompile>,
    /// The original call's gas limit (budget allocated by the parent frame).
    pub(crate) gas_limit: u64,
    /// Gas consumed by `init_subcall` (ABI decoding, validation).
    pub(crate) init_subcall_gas_overhead: u64,
    /// The original call's return memory offset.
    pub(crate) return_memory_offset: std::ops::Range<usize>,
    /// Opaque state carried from `init_subcall` to `complete_subcall`.
    pub(crate) continuation_data: SubcallContinuationData,
}

/// Specifies which addresses are authorized to call a subcall precompile.
#[derive(Debug, Clone)]
pub enum AllowedCallers {
    /// Any address may call this precompile.
    Unrestricted,
    /// Only the specified addresses may call this precompile.
    Only(HashSet<Address>),
}

impl AllowedCallers {
    /// Returns `true` if the given caller is authorized.
    pub fn is_allowed(&self, caller: &Address) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Only(set) => set.contains(caller),
        }
    }
}

/// A registered subcall precompile with its caller restrictions.
#[derive(Clone)]
struct SubcallRegistration {
    /// The subcall precompile implementation
    precompile: Arc<dyn SubcallPrecompile>,
    /// Hardcoded set of addresses allowed to call this subcall precompile.
    /// Enforced in frame_init before calling `init_subcall`.
    allowed_callers: AllowedCallers,
}

/// Registry of subcall-capable precompiles.
///
/// Maps precompile addresses to their implementations and caller restrictions.
/// Built by `ArcEvmFactory` and shared (via `Arc`) across all EVM instances for a block.
#[derive(Default, Clone)]
pub struct SubcallRegistry {
    precompiles: HashMap<Address, SubcallRegistration>,
}

impl SubcallRegistry {
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a subcall precompile at the given address.
    pub fn register(
        &mut self,
        address: Address,
        precompile: Arc<dyn SubcallPrecompile>,
        allowed_callers: AllowedCallers,
    ) {
        self.precompiles.insert(
            address,
            SubcallRegistration {
                precompile,
                allowed_callers,
            },
        );
    }

    /// Looks up a subcall precompile by address.
    ///
    /// Returns `None` if the address is not a registered subcall precompile.
    pub fn get(&self, address: &Address) -> Option<(&Arc<dyn SubcallPrecompile>, &AllowedCallers)> {
        self.precompiles
            .get(address)
            .map(|r| (&r.precompile, &r.allowed_callers))
    }
}

impl std::fmt::Debug for SubcallContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubcallContinuation")
            .field("return_memory_offset", &self.return_memory_offset)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for SubcallRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubcallRegistry")
            .field("addresses", &self.precompiles.keys().collect::<Vec<_>>())
            .finish()
    }
}
