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

pragma solidity ^0.8.29;

/**
 * @title Addresses
 * @notice Shared constants for system contract addresses
 */
library Addresses {
    // ============ System Contracts ============
    address internal constant FIAT_TOKEN_PROXY = 0x3600000000000000000000000000000000000000;
    address internal constant PROTOCOL_CONFIG = 0x3600000000000000000000000000000000000001;
    address internal constant VALIDATOR_REGISTRY = 0x3600000000000000000000000000000000000002;
    address internal constant PERMISSIONED_MANAGER = 0x3600000000000000000000000000000000000003;

    // ============ Precompiles ============
    address internal constant NATIVE_COIN_AUTHORITY = 0x1800000000000000000000000000000000000000;
    address internal constant NATIVE_COIN_CONTROL = 0x1800000000000000000000000000000000000001;
    address internal constant SYSTEM_ACCOUNTING = 0x1800000000000000000000000000000000000002;
    address internal constant CALL_FROM = 0x1800000000000000000000000000000000000003;

    // ============ Predeployed Contracts ============
    address internal constant DETERMINISTIC_DEPLOYER_PROXY = 0x4e59b44847b379578588920cA78FbF26c0B4956C;
    address internal constant MULTICALL3 = 0xcA11bde05977b3631167028862bE2a173976CA11;
    address internal constant MULTICALL3_FROM = 0xEb7cc06E3D3b5F9F9a5fA2B31B477ff72bB9c8b6;
    address internal constant MEMO = 0x9702466268ccF55eAB64cdf484d272Ac08d3b75b;

    // ============ Helpers ============

    function _hasSystemAddressPrefix(address account) internal pure returns (bool) {
        // The Arc system address range starts at 0x3600...0000.
        // Addresses are 160 bits wide; we isolate the top 12 bits by right-shifting
        // by 148 (= 160 - 12) and comparing against 0x360.
        return (uint160(account) >> 148) == 0x360;
    }
}
