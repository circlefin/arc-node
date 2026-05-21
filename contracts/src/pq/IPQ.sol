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

/// @title IPQ — Experimental post-quantum cryptography precompile interface
/// @notice Exposes post-quantum cryptographic primitives for early integrations.
///         Additional algorithms may be added in future hardforks.
interface IPQ {
    /// @notice Verify an SLH-DSA-SHA2-128s signature (FIPS 205).
    /// @dev Since PQ signatures are still very new, we recommend not to solely rely on them for
    ///      authentication, but pair them with classical signatures. Gas cost: 230,000 base + 6
    ///      per 32-byte word of message (same rate as KECCAK256).
    /// @param vk  Verifying key (32 bytes)
    /// @param message Message that was signed
    /// @param sig Signature (7856 bytes)
    /// @return    True if the signature is valid
    function verifySlhDsaSha2128s(bytes memory vk, bytes memory message, bytes memory sig) external view returns (bool);
}
