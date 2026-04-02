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

/// @title GasGuzzler
/// @notice Contract for simulating heavy computation and gas-intensive workloads
contract GasGuzzler {
    // ============ Storage ============

    /// @notice Dummy variable for gas guzzling operations
    uint256 public dummy;

    /// @notice Storage mapping for read/write tests
    mapping(uint256 => uint256) public storageMap;

    /// @notice Total number of storage reads performed
    uint256 public totalRead;

    /// @notice Total number of storage writes performed
    uint256 public totalWrite;

    // ============ Gas Guzzler Functions ============

    /// @notice Consumes gas until gasRemaining is left
    /// @param gasRemaining Target gas to leave remaining
    function guzzle(uint256 gasRemaining) external payable {
        uint256 counter;
        while (gasleft() > gasRemaining) {
            counter++;
        }
    }

    /// @notice Consumes gas by forcing a revert in static context, then loops
    /// @param gasRemaining Target gas to leave remaining
    function guzzle2(uint256 gasRemaining) external payable {
        // call sstore in static context will revert and consume all gas
        (bool success,) = address(this).staticcall{gas: gasleft() - gasRemaining}(
            abi.encodeWithSelector(this.setDummy.selector, block.timestamp)
        );
        // just for eliminating unused variable warning
        uint256 counter = success ? 1 : 0;
        while (gasleft() > gasRemaining) {
            counter++;
        }
    }

    /// @notice Set dummy variable (used by guzzle2 to trigger revert in static context)
    /// @param newDummy New value to set
    function setDummy(uint256 newDummy) external {
        assembly {
            sstore(dummy.slot, newDummy)
        }
    }

    // ================ Recursive Hashing ================

    /// @notice Perform CPU-intensive repeated hashing
    /// @param iterations Number of hash iterations to perform
    function hashLoop(uint256 iterations) external pure returns (bytes32 result) {
        for (uint256 i = 0; i < iterations; i++) {
            result = keccak256(abi.encodePacked(result, i));
        }
    }

    // =================== Storage Read ===================

    /// @notice Read storage slots in pseudo-random order - maximizes cold SLOAD costs
    /// @param iterations Number of storage slots to read
    function storageRead(uint256 iterations) external returns (uint256 result) {
        if (iterations == 0) return 0;

        uint256 baseIndex = uint256(keccak256(abi.encodePacked(msg.sender, totalRead)));
        unchecked {
            for (uint256 i = 0; i < iterations; i++) {
                result ^= storageMap[baseIndex + i];
            }
            totalRead += iterations;
        }
    }

    // ================== Storage Write ===================

    /// @notice Write to storage slots in pseudo-random order - maximizes cold SSTORE costs
    /// @param iterations Number of storage slots to write
    function storageWrite(uint256 iterations) external returns (uint256 result) {
        if (iterations == 0) return 0;

        uint256 baseIndex = uint256(keccak256(abi.encodePacked(msg.sender, totalWrite)));
        unchecked {
            for (uint256 i = 0; i < iterations; i++) {
                result ^= baseIndex + i;
                storageMap[baseIndex + i] = result;
            }
            totalWrite += iterations;
        }
    }
}
