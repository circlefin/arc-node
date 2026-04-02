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

import {IPermissionedValidatorManager} from "./interfaces/IPermissionedValidatorManager.sol";
import {IValidatorRegistry, Validator} from "./interfaces/IValidatorRegistry.sol";
import {Controller} from "./roles/Controller.sol";
import {ValidatorRegisterer} from "./roles/ValidatorRegisterer.sol";
import {Pausable} from "../common/roles/Pausable.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";

/**
 * @title PermissionedValidatorManager
 * @notice Manages validator registration with role-based access control using a three-tier architecture:
 *         Owner → Controllers → ValidatorRegisterers → Validators
 * @dev Implements {IPermissionedValidatorManager} with delegation and access control patterns.
 * @dev Controller and ValidatorRegisterer roles use ERC-7201 storage pattern
 */
contract PermissionedValidatorManager is IPermissionedValidatorManager, Controller, ValidatorRegisterer, Pausable {
    // ============ Errors ============

    /// @notice Thrown when owner address is zero
    error ZeroOwnerAddress();
    /// @notice Thrown when voting power exceeds controller limitation
    error VotingPowerExceedsLimit(uint64 limit);

    // ============ Constants ============

    /// @dev The underlying validator registry that this contract manages
    /// @dev This contract should be set as the owner of the registry for proper access control
    IValidatorRegistry public immutable REGISTRY;
    /// @notice Default voting power assigned to newly registered validators
    uint64 public constant DEFAULT_VOTING_POWER = 0;

    // ============ Events ============

    /**
     * @notice Emitted when the underlying Registry Ownership transfer has started.
     */
    event RegistryOwnerTransferStarted(address indexed newOwner);

        /**
     * @notice Emitted when the underlying Registry Ownership transfer has completed.
     */
    event RegistryOwnerTransferCompleted();

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(IValidatorRegistry validatorRegistry) {
        REGISTRY = validatorRegistry;
        _disableInitializers();
    }

    /**
     * @notice Initialize the PermissionedValidatorManager (for proxy deployment)
     * @param initialOwner The initial owner of the contract
     * @param initialPauser The address allowed to pause/unpause
     */
    function initialize(address initialOwner, address initialPauser) external initializer {
        require(initialOwner != address(0), ZeroOwnerAddress());
        require(initialPauser != address(0), ZeroPauserAddress());

        __Ownable2Step_init();
        _transferOwnership(initialOwner);

        PausableStorage storage $ = _getPausableStorage();
        $.pauser = initialPauser;
    }

    // ============ IPermissionedValidatorManager Implementation ============
    /**
     * @notice See {IPermissionedValidatorManager-registerValidator}.
     */
    function registerValidator(bytes memory publicKey) external onlyValidatorRegisterer whenNotPaused returns (uint256) {
        // Delegate to the underlying registry with a default voting power
        return REGISTRY.registerValidator(publicKey, DEFAULT_VOTING_POWER);
    }

    /**
     * @notice See {IPermissionedValidatorManager-activateValidator}.
     */
    function activateValidator() external onlyController whenNotPaused {
        ControllerStorage storage $ = _getControllerStorage();
        // Get the registrationId that this controller is assigned to manage
        uint256 registrationId = $.registrationOf[msg.sender];

        // Delegate to the underlying registry with the controller's assigned validator
        REGISTRY.activateValidator(registrationId);
    }

    /**
     * @notice See {IPermissionedValidatorManager-removeValidator}.
     */
    function removeValidator() external onlyController whenNotPaused {
        ControllerStorage storage $ = _getControllerStorage();
        // Get the registrationId that this controller is assigned to manage
        uint256 registrationId = $.registrationOf[msg.sender];

        // Delegate to the underlying registry with the controller's assigned validator
        REGISTRY.removeValidator(registrationId);
    }

    /**
     * @notice See {IPermissionedValidatorManager-updateValidatorVotingPower}.
     */
    function updateValidatorVotingPower(uint64 newVotingPower) external onlyController whenNotPaused {
        ControllerStorage storage $ = _getControllerStorage();
        // Get the registrationId that this controller is assigned to manage
        uint256 registrationId = $.registrationOf[msg.sender];

        uint64 votingPowerLimit = $.votingPowerLimitOf[msg.sender];
        // Enforce per-controller voting power limitation
        if (newVotingPower > votingPowerLimit) {
            revert VotingPowerExceedsLimit(votingPowerLimit);
        }

        // Delegate to the underlying registry with the controller's assigned validator
        REGISTRY.updateValidatorVotingPower(registrationId, newVotingPower);
    }

    /**
     * @notice See {IPermissionedValidatorManager-getValidator}.
     */
    function getValidator(address controller) external view returns (Validator memory) {
        ControllerStorage storage $ = _getControllerStorage();
        // Get the registrationId that this controller is assigned to manage
        uint256 registrationId = $.registrationOf[controller];

        // Delegate to the underlying registry with the controller's assigned validator
        return REGISTRY.getValidator(registrationId);
    }

    /**
     * @notice Updates the underlying Registry Owner
     */
    function transferRegistryOwner(address newOwner) external onlyOwner {
        require(newOwner != address(0), ZeroOwnerAddress());

        Ownable2StepUpgradeable(address(REGISTRY)).transferOwnership(newOwner);
        emit RegistryOwnerTransferStarted(newOwner);
    }

    /**
     * @notice Accepts ownership of the ValidatorRegistry
     * @dev Call this after ValidatorRegistry.transferOwnership(thisContract) has been called
     * @dev This contract must be the pending owner of the ValidatorRegistry
     */
    function acceptRegistryOwnership() external onlyOwner {
        Ownable2StepUpgradeable(address(REGISTRY)).acceptOwnership();
        emit RegistryOwnerTransferCompleted();
    }
}
