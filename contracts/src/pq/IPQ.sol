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
pragma solidity ^0.8.29;

/// @title IPQ — Post-Quantum cryptography precompile interface
/// @notice Exposes post-quantum cryptographic primitives to on-chain callers.
///         Additional algorithms may be added in future hardforks.
interface IPQ {
    /// @notice Verify an SLH-DSA-SHA2-128s signature (FIPS 205).
    /// @dev Gas cost: 230,000 base + 6 per 32-byte word of msg (same rate as KECCAK256).
    /// @param vk  Verifying key (32 bytes)
    /// @param msg Message that was signed
    /// @param sig Signature (7856 bytes)
    /// @return    True if the signature is valid
    function verifySlhDsaSha2128s(bytes memory vk, bytes memory msg, bytes memory sig) external view returns (bool);
}
