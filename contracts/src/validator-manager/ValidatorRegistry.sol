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

import {IValidatorRegistry, Validator, ValidatorStatus} from "./interfaces/IValidatorRegistry.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
import {EnumerableSet} from "@openzeppelin/contracts/utils/structs/EnumerableSet.sol";

/**
 * @title ValidatorRegistry
 * @notice Registry for managing validator registrations
 */
contract ValidatorRegistry is IValidatorRegistry, Ownable2StepUpgradeable {
    using EnumerableSet for EnumerableSet.UintSet;

    // ============ Errors ============

    /// @notice Thrown when the validator public key is already registered
    error ValidatorAlreadyRegistered(bytes32 publicKeyHash);

    /// @notice Thrown when a referenced registrationId is invalid
    error InvalidRegistrationId(uint256 registrationId);

    /// @notice Thrown for invalid public key formats
    error InvalidPublicKeyFormat();

    /// @notice Thrown for invalid voting power updates
    error InvalidVotingPowerUpdate();

    /// @notice Thrown when an update would leave the active validator set without any voting power
    error InvalidValidatorSet();

    // ============ Type Declarations ============

    /// @custom:storage-location erc7201:arc.storage.ValidatorRegistry
    struct ValidatorRegistryStorage {
        /// @notice Maps registration IDs to validator data including status, public key, and voting power
        mapping(uint256 => Validator) _validatorsByRegistrationId;
        /// @notice Enumerable set to track active validator registrationIds
        EnumerableSet.UintSet _activeValidatorRegistrations;
        /// @notice To check for duplicate registered validator public keys
        mapping(bytes32 => bool) _registeredPublicKeys;
        /// @notice Counter tracking the next available registration ID for new validator registrations, starting from 1
        uint256 _nextRegistrationId;
    }

    // ============ Constants ============

    // Ed25519 public key length in bytes
    uint256 constant ED25519_PUBLIC_KEY_LENGTH = 32;

    // keccak256(abi.encode(uint256(keccak256("arc.storage.ValidatorRegistry")) - 1)) & ~bytes32(uint256(0xff));
    bytes32 public constant VALIDATOR_REGISTRY_STORAGE_LOCATION =
        0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200;

    /**
     * @dev Constructor for implementation contract - disables initializers to prevent misuse
     */
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            VALIDATOR_REGISTRY_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.ValidatorRegistry")) - 1)) & ~bytes32(uint256(0xff))
        );

        // Disable initializers on implementation contract
        _disableInitializers();
    }

    // ============ External Functions  ============

    /**
     * @inheritdoc IValidatorRegistry
     */
    function registerValidator(bytes memory publicKey, uint64 votingPower)
        public
        onlyOwner
        returns (uint256 registrationId)
    {
        // Validate public key length
        require(publicKey.length == ED25519_PUBLIC_KEY_LENGTH, InvalidPublicKeyFormat());

        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();

        bytes32 publicKeyHash;
        assembly {
            publicKeyHash := keccak256(add(publicKey, 0x20), mload(publicKey))
        }
        // Check if public key is already registered
        require(!$._registeredPublicKeys[publicKeyHash], ValidatorAlreadyRegistered(publicKeyHash));

        registrationId = $._nextRegistrationId++;

        $._validatorsByRegistrationId[registrationId] =
            Validator({status: ValidatorStatus.Registered, publicKey: publicKey, votingPower: votingPower});
        $._registeredPublicKeys[publicKeyHash] = true;
        // Emit event for validator registration
        emit ValidatorRegistered(registrationId, votingPower, publicKey);
        return registrationId;
    }

    /**
     * @inheritdoc IValidatorRegistry
     */
    function activateValidator(uint256 registrationId) external onlyOwner {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        Validator storage validatorInfo = $._validatorsByRegistrationId[registrationId];
        require(validatorInfo.status == ValidatorStatus.Registered, InvalidRegistrationId(registrationId));

        validatorInfo.status = ValidatorStatus.Active;
        $._activeValidatorRegistrations.add(registrationId);
        emit ValidatorActivated(registrationId, validatorInfo.votingPower);
    }

    /**
     * @inheritdoc IValidatorRegistry
     */
    function removeValidator(uint256 registrationId) external onlyOwner {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        Validator storage validatorInfo = $._validatorsByRegistrationId[registrationId];

        // Ensure validator is known
        require(validatorInfo.status != ValidatorStatus.Unknown, InvalidRegistrationId(registrationId));

        if (validatorInfo.status == ValidatorStatus.Active && validatorInfo.votingPower > 0) {
            require(_countActiveValidatorsWithPositiveVotingPower($) > 1, InvalidValidatorSet());
        }

        bytes memory publicKey = validatorInfo.publicKey;
        bytes32 publicKeyHash;
        assembly {
            publicKeyHash := keccak256(add(publicKey, 0x20), mload(publicKey))
        }
        uint64 votingPower = validatorInfo.votingPower;

        // Prune state
        $._activeValidatorRegistrations.remove(registrationId);
        delete $._validatorsByRegistrationId[registrationId];
        delete $._registeredPublicKeys[publicKeyHash];

        // Emit event
        emit ValidatorRemoved(registrationId, votingPower);
    }

    /**
     * @inheritdoc IValidatorRegistry
     * @dev Setting voting power to 0 effectively deactivates the validator without removing it from storage.
     *      The validator remains registered but will have no influence in consensus.
     */
    function updateValidatorVotingPower(uint256 registrationId, uint64 newVotingPower) external onlyOwner {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        Validator storage validatorInfo = $._validatorsByRegistrationId[registrationId];

        // Ensure validator is known
        require(validatorInfo.status != ValidatorStatus.Unknown, InvalidRegistrationId(registrationId));

        uint64 oldVotingPower = validatorInfo.votingPower;
        require(oldVotingPower != newVotingPower, InvalidVotingPowerUpdate());

        if (validatorInfo.status == ValidatorStatus.Active && oldVotingPower > 0 && newVotingPower == 0) {
            require(_countActiveValidatorsWithPositiveVotingPower($) > 1, InvalidValidatorSet());
        }

        // Update voting power (setting to 0 effectively deactivates the validator)
        validatorInfo.votingPower = newVotingPower;

        // Emit event
        emit ValidatorVotingPowerUpdated(registrationId, oldVotingPower, newVotingPower);
    }

    // ============ View Functions ============

    /**
     * @notice Retrieves validator information by registration ID
     * @param registrationId The unique registration ID of the validator
     * @return Validator memory struct containing validator details including status, public key, and voting power
     */
    function getValidator(uint256 registrationId) external view returns (Validator memory) {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        return $._validatorsByRegistrationId[registrationId];
    }

    /**
     * @notice Returns all active validators
     * @dev This function is called by the consensus layer (malachite) to retrieve the current active validator set
     */
    function getActiveValidatorSet() external view returns (Validator[] memory activeValidators) {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();

        uint256 activeCount = $._activeValidatorRegistrations.length();
        activeValidators = new Validator[](activeCount);

        // Gas optimization: ++i increments directly without the temporary copy
        for (uint256 i = 0; i < activeCount; ++i) {
            uint256 registrationId = $._activeValidatorRegistrations.at(i);
            activeValidators[i] = $._validatorsByRegistrationId[registrationId];
        }

        return activeValidators;
    }

    /**
     * @notice Returns count of active validators with voting power > 0
     * @return count Number of active validators with positive voting power
     */
    function getActiveValidatorsWithPositiveVotingPowerCount() external view override returns (uint256 count) {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        return _countActiveValidatorsWithPositiveVotingPower($);
    }

    /**
     * @notice Returns the next registration ID that will be assigned
     * @return The next available registration ID
     */
    function getNextRegistrationId() external view returns (uint256) {
        ValidatorRegistryStorage storage $ = _getValidatorRegistryStorage();
        return $._nextRegistrationId;
    }

    // ============ Internal Functions ============

    /// @dev Counts active validators with voting power > 0 by scanning the active registration set.
    function _countActiveValidatorsWithPositiveVotingPower(ValidatorRegistryStorage storage $)
        internal
        view
        returns (uint256 count)
    {
        uint256 registrationCount = $._activeValidatorRegistrations.length();

        for (uint256 i = 0; i < registrationCount; ++i) {
            uint256 activeRegistrationId = $._activeValidatorRegistrations.at(i);
            if ($._validatorsByRegistrationId[activeRegistrationId].votingPower > 0) {
                ++count;
            }
        }
    }

    /**
     * @dev Returns a storage pointer to the ValidatorRegistry storage struct using ERC-7201 pattern.
     *      This prevents storage collisions in upgradeable contracts by using a deterministic slot.
     */
    function _getValidatorRegistryStorage() internal pure returns (ValidatorRegistryStorage storage $) {
        assembly {
            $.slot := VALIDATOR_REGISTRY_STORAGE_LOCATION
        }
    }
}
