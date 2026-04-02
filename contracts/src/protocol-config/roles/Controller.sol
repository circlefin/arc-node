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
 * @notice Base contract which allows children to manage controller role for protocol configuration updates
 * @dev Uses ERC-7201 namespaced storage pattern for upgrade safety.
 */
abstract contract Controller is Ownable2StepUpgradeable {
    // ============ Errors ============

    /// @notice Thrown when controller address is zero
    error ZeroControllerAddress();

    /// @notice Thrown when caller is not the controller
    error CallerIsNotController();

    // ============ Storage (ERC-7201) ============

    /// @custom:storage-location erc7201:arc.storage.ProtocolConfigController
    struct ProtocolConfigControllerStorage {
        /// @notice Current controller address (can call update functions)
        address controller;
    }

    // keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfigController")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant PROTOCOL_CONFIG_CONTROLLER_STORAGE_LOCATION =
        0x958f8fec699b51a1249f513eceda5429078000657f74abd1721bba363087af00;

    function _getProtocolConfigControllerStorage() internal pure returns (ProtocolConfigControllerStorage storage $) {
        assembly {
            $.slot := PROTOCOL_CONFIG_CONTROLLER_STORAGE_LOCATION
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            PROTOCOL_CONFIG_CONTROLLER_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfigController")) - 1)) & ~bytes32(uint256(0xff))
        );
    }

    // ============ Events ============

    /**
     * @notice Emitted when controller is updated
     * @param newController new controller address
     */
    event ControllerUpdated(address indexed newController);

    // ============ Modifiers ============

    /**
     * @dev Modifier to restrict access to controller only
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
        ProtocolConfigControllerStorage storage $ = _getProtocolConfigControllerStorage();
        require(msg.sender == $.controller, CallerIsNotController());
    }

    /**
     * @notice Returns the current controller address
     * @return The address of the current controller
     */
    function controller() public view virtual returns (address) {
        ProtocolConfigControllerStorage storage $ = _getProtocolConfigControllerStorage();
        return $.controller;
    }

    /**
     * @notice Updates the controller address
     * @param newController The new controller address (cannot be zero address)
     * @dev Only callable by owner
     */
    function updateController(address newController) external virtual onlyOwner {
        require(newController != address(0), ZeroControllerAddress());

        ProtocolConfigControllerStorage storage $ = _getProtocolConfigControllerStorage();
        $.controller = newController;
        emit ControllerUpdated(newController);
    }
}
