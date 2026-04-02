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
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";

/**
 * @title Denylist
 */
contract Denylist is Initializable, Ownable2StepUpgradeable {
    // ============ Errors ============

    /// @notice Thrown when a denylister attempts to denylist the owner
    error CannotDenylistOwner();
    /// @notice Thrown when caller is not a denylister
    error CallerIsNotDenylister();
    /// @notice Thrown when address is zero
    error ZeroAddress();

    // ============ Storage (ERC-7201) ============

    /// @custom:storage-location erc7201:arc.storage.Denylist.v1
    struct DenylistStorage {
        mapping(address => bool) denylisted; // baseSlot + 0
        mapping(address => bool) denylisters; // baseSlot + 1
    }

    /// @dev keccak256(abi.encode(uint256(keccak256("arc.storage.Denylist.v1")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 public constant DENYLIST_STORAGE_LOCATION =
        0x1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00;

    // ============ Events ============

    event Denylisted(address indexed account);
    event UnDenylisted(address indexed account);
    event DenylisterAdded(address indexed account);
    event DenylisterRemoved(address indexed account);

    // ============ Modifiers ============

    modifier onlyDenylister() {
        DenylistStorage storage $ = _getDenylistStorage();
        if (!$.denylisters[msg.sender]) revert CallerIsNotDenylister();
        _;
    }

    // ============ Constructor / Initializer ============

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
        assert(
            DENYLIST_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.Denylist.v1")) - 1))
                    & ~bytes32(uint256(0xff))
        );
    }

    /**
     * @notice Initialize the Denylist (for proxy deployment).
     * @param initialOwner The initial owner of the contract.
     */
    function initialize(address initialOwner) external initializer {
        if (initialOwner == address(0)) revert ZeroAddress();
        __Ownable2Step_init();
        _transferOwnership(initialOwner);
    }

    // ============ Denylist / UnDenylist ============

    /**
     * @notice Add addresses to the denylist. Caller must be a denylister. Owner cannot be denylisted.
     * @param accounts Addresses to denylist.
     */
    function denylist(address[] calldata accounts) external onlyDenylister {
        address _owner = owner();
        DenylistStorage storage $ = _getDenylistStorage();
        uint256 _accounts_length = accounts.length;
        for (uint256 i = 0; i < _accounts_length; ++i) {
            address account = accounts[i];
            if (account == _owner) revert CannotDenylistOwner();
            if (!$.denylisted[account]) {
                $.denylisted[account] = true;
                emit Denylisted(account);
            }
        }
    }

    /**
     * @notice Remove addresses from the denylist. Caller must be a denylister.
     * @param accounts Addresses to remove from the denylist.
     */
    function unDenylist(address[] calldata accounts) external onlyDenylister {
        DenylistStorage storage $ = _getDenylistStorage();
        uint256 _accounts_length = accounts.length;
        for (uint256 i = 0; i < _accounts_length; ++i) {
            address account = accounts[i];
            if ($.denylisted[account]) {
                delete $.denylisted[account];
                emit UnDenylisted(account);
            }
        }
    }

    // ============ View functions ============

    /**
     * @notice Check if an address is denylisted.
     * @param account Address to check.
     * @return True if denylisted, false otherwise.
     */
    function isDenylisted(address account) external view returns (bool) {
        DenylistStorage storage $ = _getDenylistStorage();
        return $.denylisted[account];
    }

    /**
     * @notice Check if an address is a denylister.
     * @param account Address to check.
     * @return True if denylister, false otherwise.
     */
    function isDenylister(address account) external view returns (bool) {
        DenylistStorage storage $ = _getDenylistStorage();
        return $.denylisters[account];
    }

    // ============ Owner: manage denylisters ============

    /**
     * @notice Add a denylister. Only owner.
     * @param account Address to grant denylister role.
     */
    function addDenylister(address account) external onlyOwner {
        if (account == address(0)) revert ZeroAddress();
        DenylistStorage storage $ = _getDenylistStorage();
        if (!$.denylisters[account]) {
            $.denylisters[account] = true;
            emit DenylisterAdded(account);
        }
    }

    /**
     * @notice Remove a denylister. Only owner.
     * @param account Address to revoke denylister role.
     */
    function removeDenylister(address account) external onlyOwner {
        if (account == address(0)) revert ZeroAddress();
        DenylistStorage storage $ = _getDenylistStorage();
        if ($.denylisters[account]) {
            delete $.denylisters[account];
            emit DenylisterRemoved(account);
        }
    }

    // ============ Internal ============

    function _getDenylistStorage() private pure returns (DenylistStorage storage $) {
        assembly {
            $.slot := DENYLIST_STORAGE_LOCATION
        }
    }
}
