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

/// @notice Validator status.
enum ValidatorStatus {
    Unknown, // Initial state or validator that hasn't been registered
    Registered, // Validator has been registered but not yet active in the network
    Active // Validator is currently active and participating in consensus

}

/**
 * @notice Contains the status of a Validator.
 * @param status The current status of the validator (Unknown, Registered, or Active)
 * @param publicKey The public key of the validator
 * @param votingPower The current voting power of the validator.
 */
struct Validator {
    ValidatorStatus status;
    bytes publicKey;
    uint64 votingPower;
}

/**
 * @dev ValidatorRegistry interface
 */
interface IValidatorRegistry {
    // ============ Events ============
    /// @notice Emitted when a validator is registered.
    event ValidatorRegistered(uint256 indexed registrationId, uint64 votingPower, bytes publicKey);

    /// @notice Emitted when a validator is activated.
    event ValidatorActivated(uint256 indexed registrationId, uint64 votingPower);

    /// @notice Emitted when a validator is removed.
    event ValidatorRemoved(uint256 indexed registrationId, uint64 votingPower);

    /// @notice Emitted when a validator votingPower update is updated.
    event ValidatorVotingPowerUpdated(uint256 indexed registrationId, uint64 oldVotingPower, uint64 newVotingPower);

    // ============ Functions ============
    function registerValidator(bytes memory publicKey, uint64 votingPower) external returns (uint256 registrationId);

    function activateValidator(uint256 registrationId) external;

    function removeValidator(uint256 registrationId) external;

    function updateValidatorVotingPower(uint256 registrationId, uint64 newVotingPower) external;

    /**
     * @notice Returns the validator registration info, including the validator, public key, and voting power.
     * @param registrationId The unique identifier for the validator registration
     */
    function getValidator(uint256 registrationId) external view returns (Validator memory);

    /**
     * @notice Returns all validators that are currently active
     * @return activeValidators Array of active validator structs
     */
    function getActiveValidatorSet() external view returns (Validator[] memory activeValidators);

    /**
     * @notice Returns count of active validators with voting power > 0
     * @return count Number of active validators with positive voting power
     */
    function getActiveValidatorsWithPositiveVotingPowerCount() external view returns (uint256 count);
}
