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

import {Test} from "forge-std/Test.sol";
import {ValidatorRegistry} from "../../src/validator-manager/ValidatorRegistry.sol";

contract TestUtils is Test {
    // ValidatorRegistry events
    event ValidatorRegistered(uint256 indexed registrationId, uint64 votingPower, bytes publicKey);
    event ValidatorActivated(uint256 indexed registrationId, uint64 votingPower);
    event ValidatorRemoved(uint256 indexed registrationId, uint64 votingPower);
    event ValidatorVotingPowerUpdated(uint256 indexed registrationId, uint64 oldVotingPower, uint64 newVotingPower);

    /// @dev Helper function to generate a valid Ed25519 public key for testing
    function generateEd25519PublicKey(uint256 seed) public pure returns (bytes memory publicKey) {
        // Generate a deterministic 32-byte Ed25519 public key
        bytes32 keyBytes = keccak256(abi.encodePacked("ed25519_test_key", seed));
        publicKey = abi.encodePacked(keyBytes);
    }

    // Helper function to set owner for ValidatorRegistry (simulating genesis initialization)
    function _setRegistryOwner(ValidatorRegistry registry, address newOwner) internal {
        // ValidatorRegistry now uses Ownable2StepUpgradeable, so owner is stored in ERC-7201 slot
        // ERC-7201 slot for "openzeppelin.storage.Ownable"
        bytes32 ownableSlot = 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300;
        vm.store(address(registry), ownableSlot, bytes32(uint256(uint160(newOwner))));

        // Initialize the _nextRegistrationID in the ValidatorRegistry storage
        // _nextRegistrationID is at offset 4 within the ValidatorRegistryStorage struct
        bytes32 registryBaseSlot = registry.VALIDATOR_REGISTRY_STORAGE_LOCATION();
        bytes32 nextRegistrationIdSlot = bytes32(uint256(registryBaseSlot) + 4);
        vm.store(address(registry), nextRegistrationIdSlot, bytes32(uint256(1))); // _nextRegistrationID = 1
    }
}
