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

import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";

/**
 * @title Pausable
 * @notice Base contract which allows children to implement an emergency stop mechanism
 * @dev Forked from https://github.com/circlefin/stablecoin-evm/blob/c8c31b249341bf3ffb2e8dbff41977c392a260c5/contracts/v1/Pausable.sol
 * @dev Features:
 * - Separate pauser role (different from owner)
 * - Owner can update pauser address
 * - Only pauser can pause/unpause
 * @dev Uses ERC-7201 namespaced storage pattern for upgrade safety.
 */
abstract contract Pausable is Ownable2StepUpgradeable {

    // ============ Errors ============

    /// @notice Thrown when pauser address is zero
    error ZeroPauserAddress();

    /// @notice Thrown when caller is not the pauser
    error CallerIsNotPauser();

    /// @notice Thrown when contract is paused
    error ContractPaused();

    // ============ Storage (ERC-7201) ============

    /// @custom:storage-location erc7201:arc.storage.Pausable
    struct PausableStorage {
        /// @notice Current pauser address
        address pauser;
        /// @notice Whether the contract is paused
        bool paused;
    }

    // keccak256(abi.encode(uint256(keccak256("arc.storage.Pausable")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant PAUSABLE_STORAGE_LOCATION =
        0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00;

    function _getPausableStorage() internal pure returns (PausableStorage storage $) {
        assembly {
            $.slot := PAUSABLE_STORAGE_LOCATION
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            PAUSABLE_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.Pausable")) - 1)) & ~bytes32(uint256(0xff))
        );
    }

    // ============ Events ============

    /**
     * @notice Emitted when contract is paused
     */
    event Pause();

    /**
     * @notice Emitted when contract is unpaused
     */
    event Unpause();

    /**
     * @notice Emitted when pauser address is updated
     * @param newAddress new pauser address
     */
    event PauserChanged(address indexed newAddress);

    // ============ Modifiers ============

    /**
     * @dev Modifier to make a function callable only when the contract is not paused
     */
    modifier whenNotPaused() {
        _whenNotPaused();
        _;
    }

    /**
     * @dev Modifier to restrict access to pauser only
     */
    modifier onlyPauser() {
        _onlyPauser();
        _;
    }

    // ============ Functions ============

    /**
     * @dev Internal function to check if contract is not paused
     */
    function _whenNotPaused() internal view {
        PausableStorage storage $ = _getPausableStorage();
        require(!$.paused, ContractPaused());
    }

    /**
     * @dev Internal function to check if caller is pauser
     */
    function _onlyPauser() internal view {
        PausableStorage storage $ = _getPausableStorage();
        require(msg.sender == $.pauser, CallerIsNotPauser());
    }

    /**
     * @notice Returns the current pauser address
     * @return The address of the current pauser
     */
    function pauser() public view virtual returns (address) {
        PausableStorage storage $ = _getPausableStorage();
        return $.pauser;
    }

    /**
     * @notice Returns whether the contract is paused
     * @return True if the contract is paused, false otherwise
     */
    function paused() public view virtual returns (bool) {
        PausableStorage storage $ = _getPausableStorage();
        return $.paused;
    }

    /**
     * @notice Updates the pauser address
     * @param newPauser The address of the new pauser
     * @dev Only callable by owner
     */
    function updatePauser(address newPauser) external virtual onlyOwner {
        require(newPauser != address(0), ZeroPauserAddress());
        PausableStorage storage $ = _getPausableStorage();
        $.pauser = newPauser;
        emit PauserChanged(newPauser);
    }

    /**
     * @notice Pauses the contract
     * @dev Only callable by pauser
     */
    function pause() external virtual onlyPauser {
        PausableStorage storage $ = _getPausableStorage();
        $.paused = true;
        emit Pause();
    }

    /**
     * @notice Unpauses the contract
     * @dev Only callable by pauser
     */
    function unpause() external virtual onlyPauser {
        PausableStorage storage $ = _getPausableStorage();
        $.paused = false;
        emit Unpause();
    }
}
