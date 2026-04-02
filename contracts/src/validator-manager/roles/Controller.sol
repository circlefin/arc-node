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

import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";

/**
 * @title Controller
 * @notice Base contract which allows children to adjust the weight, or remove, an individual validator that it controls.
 * Multiple controllers can control a single validator.
 * @dev Uses ERC-7201 storage pattern for upgradeable contracts
 */
abstract contract Controller is Ownable2StepUpgradeable {
    // ============ Errors ============

    /// @notice Thrown when controller address is zero
    error ZeroControllerAddress();

    /// @notice Thrown when caller is not a controller
    error CallerIsNotController();

    /// @notice Thrown when registration ID is zero
    error RegistrationIdIsZero();

    /// @notice Thrown when controller is already configured
    error ControllerAlreadyConfigured();

    /// @notice Thrown when controller is not configured
    error ControllerNotConfigured();

    // ============ Type Declarations ============

    /// @custom:storage-location erc7201:arc.storage.PVMController
    struct ControllerStorage {
        /// @notice Records the registration ID for each controller address
        mapping(address => uint256) registrationOf;
        /// @notice Voting power limit each controller can set for their validator
        mapping(address => uint64) votingPowerLimitOf;
    }

    // ============ Constants ============

    // keccak256(abi.encode(uint256(keccak256("arc.storage.PVMController")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 public constant CONTROLLER_STORAGE_LOCATION =
        0xe90ec3add3e251bfbe914c9e482b511e91a3b187718c1dc10223f64a8a644a00;

    // ============ Constructor ============

    /**
     * @dev Constructor verifies the storage location calculation
     */
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            CONTROLLER_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.PVMController")) - 1)) & ~bytes32(uint256(0xff))
        );
    }

    // ============ Events ============
    /**
     * @notice Emitted when controller is configured
     * @param controller controller address
     * @param registrationId registrationId of the validator
     * @param votingPowerLimit voting power limit the controller can assign
     */
    event ControllerConfigured(address indexed controller, uint256 indexed registrationId, uint64 votingPowerLimit);

    /**
     * @notice Emitted when controller is removed
     * @param controller controller address removed
     */
    event ControllerRemoved(address indexed controller);

    /**
     * @notice Emitted when controller voting power cap is updated
     * @param controller controller address
     * @param newVotingPowerLimit new voting power limit for the controller
     */
    event VotingPowerLimitUpdated(address indexed controller, uint64 newVotingPowerLimit);

    // ============ Modifiers ============
    /**
     * @dev Throws if called by any account other than the controller.
     */
    modifier onlyController() {
        _onlyController();
        _;
    }

    // ============ Functions ============

    /**
     * @dev Internal function to check if caller is controller
     */
    function _onlyController() internal view {
        ControllerStorage storage $ = _getControllerStorage();
        require($.registrationOf[msg.sender] != 0, CallerIsNotController());
    }
    /**
     * @notice Assigns a controller to manage a registrationId
     * @param controller The address to grant controller permissions
     * @param registrationId The registration ID associated with the controller
     * @param votingPowerLimit The limitation of voting power the controller can set
     */
    function configureController(address controller, uint256 registrationId, uint64 votingPowerLimit) external onlyOwner {
        require(registrationId != 0, RegistrationIdIsZero());
        require(controller != address(0), ZeroControllerAddress());
        
        ControllerStorage storage $ = _getControllerStorage();
        // Prevent configuring a controller that is already configured
        require($.registrationOf[controller] == 0, ControllerAlreadyConfigured());
        
        $.registrationOf[controller] = registrationId;
        $.votingPowerLimitOf[controller] = votingPowerLimit;
        emit ControllerConfigured(controller, registrationId, votingPowerLimit);
    }

    /**
     * @notice Remove a controller
     * @param controller The address to revoke controller permissions
     */
    function removeController(address controller) external onlyOwner {
        require(controller != address(0), ZeroControllerAddress());
        
        ControllerStorage storage $ = _getControllerStorage();
        require($.registrationOf[controller] != 0, ControllerNotConfigured());
        
        delete $.registrationOf[controller];
        delete $.votingPowerLimitOf[controller];
        emit ControllerRemoved(controller);
    }

    /**
     * @notice Check if an address is a controller
     * @param controller The address to check
     * @return True if the address is a controller, false otherwise
     */
    function isController(address controller) external view returns (bool) {
        ControllerStorage storage $ = _getControllerStorage();
        return $.registrationOf[controller] != 0;
    }

    /**
     * @notice Get the registration ID for a controller
     * @param controller The address to get the registration ID for
     * @return The registration ID for the controller
     */
    function getRegistrationId(address controller) external view returns (uint256) {
        ControllerStorage storage $ = _getControllerStorage();
        return $.registrationOf[controller];
    }

    /**
     * @notice Get the voting power limit for a controller
     * @param controller The address to get the limit for
     * @return The voting power limit for the controller
     */
    function getVotingPowerLimit(address controller) external view returns (uint64) {
        ControllerStorage storage $ = _getControllerStorage();
        return $.votingPowerLimitOf[controller];
    }

    /**
     * @notice Update the voting power limit for a configured controller
     * @param controller The controller address to update
     * @param newVotingPowerLimit The new voting power limit to set
     */
    function updateVotingPowerLimit(address controller, uint64 newVotingPowerLimit) external onlyOwner {
        require(controller != address(0), ZeroControllerAddress());

        ControllerStorage storage $ = _getControllerStorage();
        require($.registrationOf[controller] != 0, ControllerNotConfigured());

        // It's valid to lower the limit even if current validator's voting power is above it.
        $.votingPowerLimitOf[controller] = newVotingPowerLimit;
        emit VotingPowerLimitUpdated(controller, newVotingPowerLimit);
    }

    // ============ Internal Functions ============

    /**
     * @dev Returns a storage pointer to the Controller storage struct using ERC-7201 pattern.
     */
    function _getControllerStorage() internal pure returns (ControllerStorage storage $) {
        assembly {
            $.slot := CONTROLLER_STORAGE_LOCATION
        }
    }
}
