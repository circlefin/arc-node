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

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";

/**
 * @title AdminUpgradeableProxy
 * @notice This contract combines an upgradeable proxy with an authorization
 * mechanism for administrative tasks.
 *
 * @dev Forked from https://github.com/OpenZeppelin/openzeppelin-contracts/blob/7b74442c5e87ea51dde41c7f18a209fa5154f1a4/contracts/proxy/transparent/TransparentUpgradeableProxy.sol
 * Modifications (7/31/2025):
 * - Remove the dependency on ProxyAdmin contract.
 * - Add public view functions admin() and implementation() using ERC1967Utils.
 * - Remove overridden _fallback() implementation, removing the ifAdmin check before delegating to the implementation.
 * - Add explicit public upgradeTo() and upgradeToAndCall() functions with onlyAdmin access.
 */
contract AdminUpgradeableProxy is ERC1967Proxy {
    /**
     * @dev Explicit admin gate: non-admins are forwarded via `_fallback()` while
     * admins enter the function body. Keeping the branch local to the modifier
     * (instead of relying on `_fallback()` never returning) makes the access
     * control intent clear and resilient to future changes.
     *
     * forge-lint: `unwrapped-modifier-logic` is suppressed because this inline
     * form best documents the behavior; wrapping it in a helper would satisfy
     * the lint but obscure the rationale.
     */
    /// forge-lint: disable-next-item(unwrapped-modifier-logic)
    modifier onlyAdmin() {
        if (msg.sender == ERC1967Utils.getAdmin()) {
            _;
        } else {
            _fallback();
        }
    }

    /**
     * @dev Initializes an upgradeable proxy managed by `_admin`, backed by the implementation at `_logic`, and
     * optionally initialized with `_data`.
     */
    constructor(address _logic, address _admin, bytes memory _data) payable ERC1967Proxy(_logic, _data) {
        // Set the storage value and emit an event for ERC-1967 compatibility
        ERC1967Utils.changeAdmin(_admin);
    }

    /**
     * @dev Upgrade the implementation of the proxy.
     * @dev Only the admin can call this function; other callers are delegated
     */
    function upgradeTo(address newImplementation) external virtual onlyAdmin {
        ERC1967Utils.upgradeToAndCall(newImplementation, "");
    }

    /**
     * @dev Upgrade the implementation of the proxy and call a function on the new implementation.
     * @dev Only the admin can call this function; other callers are delegated
     */
    function upgradeToAndCall(address newImplementation, bytes calldata data) external payable virtual onlyAdmin {
        ERC1967Utils.upgradeToAndCall(newImplementation, data);
    }

    /**
     * @dev Changes the admin of the proxy.
     *
     * Emits an {IERC1967-AdminChanged} event.
     */
    function changeAdmin(address newAdmin) external virtual onlyAdmin {
        ERC1967Utils.changeAdmin(newAdmin);
    }

    /**
     * @dev Returns the current implementation address.
     */
    function implementation() external view virtual returns (address) {
        return ERC1967Utils.getImplementation();
    }

    /**
     * @dev Returns the current admin.
     */
    function admin() external view virtual returns (address) {
        return ERC1967Utils.getAdmin();
    }
}
