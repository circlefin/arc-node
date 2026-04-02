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
 * @title TestImplementation
 * @notice A simple implementation contract for testing proxy functionality
 * @dev This contract provides basic functionality to test proxy patterns without
 *      being tied to specific business logic
 */
contract TestImplementation {
    // ============ Storage Variables ============

    address public owner;
    uint256 public value;
    string public name;
    bool public initialized;

    // ============ Events ============

    event ValueUpdated(uint256 oldValue, uint256 newValue);
    event NameUpdated(string oldName, string newName);
    event OwnerChanged(address oldOwner, address newOwner);
    event InitializeCalled(address owner, uint256 value, string name);

    // ============ Errors ============

    error NotOwner();
    error AlreadyInitialized();

    // ============ Modifiers ============

    modifier onlyOwner() {
        _onlyOwner();
        _;
    }

    // ============ Internal Functions ============

    function _onlyOwner() internal view {
        if (msg.sender != owner) {
            revert NotOwner();
        }
    }

    // ============ Constructor ============

    constructor() {
        // Leave uninitialized for proxy pattern
    }

    // ============ Initialization ============

    /**
     * @notice Initialize the contract (for proxy pattern)
     * @param _owner Initial owner address
     * @param _value Initial value
     * @param _name Initial name
     */
    function initialize(address _owner, uint256 _value, string memory _name) external {
        if (initialized) {
            revert AlreadyInitialized();
        }

        owner = _owner;
        value = _value;
        name = _name;
        initialized = true;

        emit InitializeCalled(_owner, _value, _name);
    }

    // ============ Public Functions ============

    /**
     * @notice Update the stored value (owner only)
     * @param _newValue New value to set
     */
    function updateValue(uint256 _newValue) external onlyOwner {
        uint256 oldValue = value;
        value = _newValue;
        emit ValueUpdated(oldValue, _newValue);
    }

    /**
     * @notice Update the stored name (owner only)
     * @param _newName New name to set
     */
    function updateName(string memory _newName) external onlyOwner {
        string memory oldName = name;
        name = _newName;
        emit NameUpdated(oldName, _newName);
    }

    /**
     * @notice Change the owner (owner only)
     * @param _newOwner New owner address
     */
    function changeOwner(address _newOwner) external onlyOwner {
        address oldOwner = owner;
        owner = _newOwner;
        emit OwnerChanged(oldOwner, _newOwner);
    }

    /**
     * @notice Get the current value (public read)
     * @return Current stored value
     */
    function getValue() external view returns (uint256) {
        return value;
    }

    /**
     * @notice Get the current name (public read)
     * @return Current stored name
     */
    function getName() external view returns (string memory) {
        return name;
    }

    /**
     * @notice Get the current owner (public read)
     * @return Current owner address
     */
    function getOwner() external view returns (address) {
        return owner;
    }

    /**
     * @notice Check if initialized (public read)
     * @return Whether contract is initialized
     */
    function isInitialized() external view returns (bool) {
        return initialized;
    }

    /**
     * @notice Payable function for testing ETH handling
     * @param _data Arbitrary data to process
     * @return success Whether operation succeeded
     */
    function processWithValue(bytes memory _data) external payable returns (bool success) {
        // Simple processing - just return true if we received ETH and data
        success = (msg.value > 0 || _data.length > 0);
        return success;
    }

    /**
     * @notice Function that always reverts (for testing revert scenarios)
     */
    function alwaysReverts() external pure {
        revert("This function always reverts");
    }

    // ============ Fallback & Receive ============

    /**
     * @notice Fallback function that accepts ETH
     */
    fallback() external payable {}

    /**
     * @notice Receive function that accepts ETH
     */
    receive() external payable {}
}
