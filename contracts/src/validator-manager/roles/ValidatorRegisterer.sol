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
 * @title ValidatorRegisterer
 * @notice Can register new validators
 * @dev Uses ERC-7201 storage pattern for upgradeable contracts
 */
abstract contract ValidatorRegisterer is Ownable2StepUpgradeable {
    // ============ Errors ============

    /// @notice Thrown when validator registerer address is zero
    error ZeroValidatorRegistererAddress();

    /// @notice Thrown when caller is not a validator registerer
    error CallerIsNotValidatorRegisterer();

    /// @notice Thrown when validator registerer is already added
    error ValidatorRegistererAlreadyAdded();

    /// @notice Thrown when validator registerer is not added
    error ValidatorRegistererNotAdded();
    // ============ Type Declarations ============

    /// @custom:storage-location erc7201:arc.storage.PVMValidatorRegisterer
    struct ValidatorRegistererStorage {
        /// @notice Role with permission to register new validators
        mapping(address => bool) validatorRegisterers;
    }

    // ============ Constants ============

    // keccak256(abi.encode(uint256(keccak256("arc.storage.PVMValidatorRegisterer")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 public constant VALIDATOR_REGISTERER_STORAGE_LOCATION =
        0x36c39aeb5f498ae36546fc14573b003abf87227a5a2df6caec16ee566f1ad800;

    // ============ Constructor ============

    /**
     * @dev Constructor verifies the storage location calculation
     */
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            VALIDATOR_REGISTERER_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.PVMValidatorRegisterer")) - 1)) & ~bytes32(uint256(0xff))
        );
    }

    // ============ Events ============
    /**
     * @notice Emitted when validatorRegisterer is set
     * @param validatorRegisterer validator registerer address
     */
    event ValidatorRegistererAdded(address indexed validatorRegisterer);

    /**
     * @notice Emitted when validatorRegisterer is removed
     * @param validatorRegisterer validator registerer address removed
     */
    event ValidatorRegistererRemoved(address indexed validatorRegisterer);

    // ============ Modifiers ============
    /**
     * @dev Throws if called by any account other than a validator registerer.
     */
    modifier onlyValidatorRegisterer() {
        _onlyValidatorRegisterer();
        _;
    }

    // ============ Functions ============

    /**
     * @dev Internal function to check if caller is validator registerer
     */
    function _onlyValidatorRegisterer() internal view {
        ValidatorRegistererStorage storage $ = _getValidatorRegistererStorage();
        require($.validatorRegisterers[msg.sender], CallerIsNotValidatorRegisterer());
    }
    /**
     * @notice Add a new validator registerer
     * @param validatorRegisterer The address to grant validator registerer permissions
     */
    function addValidatorRegisterer(address validatorRegisterer) external onlyOwner {
        require(validatorRegisterer != address(0), ZeroValidatorRegistererAddress());
        
        ValidatorRegistererStorage storage $ = _getValidatorRegistererStorage();
        // Prevent adding a validatorRegisterer that is already added
        require($.validatorRegisterers[validatorRegisterer] == false, ValidatorRegistererAlreadyAdded());
        
        $.validatorRegisterers[validatorRegisterer] = true;
        emit ValidatorRegistererAdded(validatorRegisterer);
    }

    /**
     * @notice Remove a validator registerer
     * @param validatorRegisterer The address to revoke validator registerer permissions
     */
    function removeValidatorRegisterer(address validatorRegisterer) external onlyOwner {
        require(validatorRegisterer != address(0), ZeroValidatorRegistererAddress());
        
        ValidatorRegistererStorage storage $ = _getValidatorRegistererStorage();
        require($.validatorRegisterers[validatorRegisterer], ValidatorRegistererNotAdded());
        
        $.validatorRegisterers[validatorRegisterer] = false;
        emit ValidatorRegistererRemoved(validatorRegisterer);
    }

    /**
     * @notice Check if an address is a validator registerer
     * @param validatorRegisterer The address to check
     * @return True if the address is a validator registerer, false otherwise
     */
    function isValidatorRegisterer(address validatorRegisterer) external view returns (bool) {
        ValidatorRegistererStorage storage $ = _getValidatorRegistererStorage();
        return $.validatorRegisterers[validatorRegisterer];
    }

    // ============ Internal Functions ============

    /**
     * @dev Returns a storage pointer to the ValidatorRegisterer storage struct using ERC-7201 pattern.
     */
    function _getValidatorRegistererStorage() internal pure returns (ValidatorRegistererStorage storage $) {
        assembly {
            $.slot := VALIDATOR_REGISTERER_STORAGE_LOCATION
        }
    }
}
