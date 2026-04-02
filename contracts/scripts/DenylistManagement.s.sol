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

import {Script, console} from "forge-std/Script.sol";
import {Denylist} from "../src/Denylist.sol";
import {AdminUpgradeableProxy} from "../src/proxy/AdminUpgradeableProxy.sol";
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";

/**
 * @notice Helper script for managing the Denylist contract (denylisters, denylisted addresses, ownership)
 * @dev All functions read the proxy address from DENYLIST_PROXY env var.
 *
 * Setup:
 *   export DENYLIST_PROXY=<denylist_proxy_address>
 *
 * ============ Print Functions (read-only, no key required) ============
 * Print contract info (proxy, implementation, admin, owner, pending owner):
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "printContractInfo()"
 *
 * Check if an address is denylisted:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "printIsDenylisted(address)" <account>
 *
 * Check if an address is a denylister:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "printIsDenylister(address)" <account>
 *
 * Batch-check denylisted status:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "printBatchDenylistStatus(address[])" "[addr1,addr2,...]"
 *
 * ============ Denylister Management (owner only, requires OWNER_KEY env var) ============
 * Add a denylister:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "addDenylister(address)" <account> --broadcast
 *
 * Remove a denylister:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "removeDenylister(address)" <account> --broadcast
 *
 * ============ Denylist Operations (denylister only, requires DENYLISTER_KEY env var) ============
 * Denylist addresses:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "denylist(address[])" "[addr1,addr2,...]" --broadcast
 *
 * Un-denylist addresses:
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "unDenylist(address[])" "[addr1,addr2,...]" --broadcast
 *
 * ============ Ownership Transfer (owner only, requires OWNER_KEY env var) ============
 * Transfer ownership (two-step):
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "transferOwnership(address)" <newOwner> --broadcast
 *
 * Accept ownership (new owner, requires NEW_OWNER_KEY env var):
 *   forge script scripts/DenylistManagement.s.sol --rpc-url <network> --sig "acceptOwnership()" --broadcast
 */
contract DenylistManagement is Script {

    // ============ Helpers ============

    function printContractInfo() public view {
        Denylist _denylist = _getDenylist();
        address proxy = address(_denylist);
        AdminUpgradeableProxy proxyContract = AdminUpgradeableProxy(payable(proxy));

        console.log("Denylist Contract Info:");
        console.log("proxy:", proxy);
        console.log("implementation:", proxyContract.implementation());
        console.log("proxyAdmin:", proxyContract.admin());
        console.log("owner:", _denylist.owner());
        console.log("pendingOwner:", Ownable2StepUpgradeable(proxy).pendingOwner());
    }

    function printIsDenylisted(address account) public view {
        bool denylisted = _getDenylist().isDenylisted(account);
        console.log("isDenylisted(%s): %s", account, denylisted);
    }

    function printIsDenylister(address account) public view {
        bool denylister = _getDenylist().isDenylister(account);
        console.log("isDenylister(%s): %s", account, denylister);
    }

    function printBatchDenylistStatus(address[] calldata accounts) public view {
        Denylist _denylist = _getDenylist();
        console.log("Denylist Status (%s accounts):", accounts.length);
        console.log("-------------------------");
        for (uint256 i = 0; i < accounts.length; i++) {
            console.log("%s: %s", accounts[i], _denylist.isDenylisted(accounts[i]));
        }
    }

    // ============ Denylister Management (owner only) ============

    function addDenylister(address account) public {
        Denylist _denylist = _getDenylist();
        uint256 ownerKey = _getOwnerKey();

        vm.startBroadcast(ownerKey);
        _denylist.addDenylister(account);
        vm.stopBroadcast();

        console.log("Added denylister:", account);
    }

    function removeDenylister(address account) public {
        Denylist _denylist = _getDenylist();
        uint256 ownerKey = _getOwnerKey();

        vm.startBroadcast(ownerKey);
        _denylist.removeDenylister(account);
        vm.stopBroadcast();

        console.log("Removed denylister:", account);
    }

    // ============ Denylist Operations (denylister only) ============

    function denylist(address[] calldata accounts) public {
        require(accounts.length > 0, "accounts cannot be empty");

        Denylist _denylist = _getDenylist();
        uint256 denylisterKey = _getDenylisterKey();

        _rejectProtectedAddresses(_denylist, accounts);

        vm.startBroadcast(denylisterKey);
        _denylist.denylist(accounts);
        vm.stopBroadcast();

        console.log("Denylisted %s address(es)", accounts.length);
    }

    function unDenylist(address[] calldata accounts) public {
        require(accounts.length > 0, "accounts cannot be empty");

        Denylist _denylist = _getDenylist();
        uint256 denylisterKey = _getDenylisterKey();

        vm.startBroadcast(denylisterKey);
        _denylist.unDenylist(accounts);
        vm.stopBroadcast();

        console.log("Un-denylisted %s address(es)", accounts.length);
    }

    // ============ Ownership Transfer ============

    function transferOwnership(address newOwner) public {
        require(newOwner != address(0), "new owner cannot be zero address");

        address proxy = address(_getDenylist());
        uint256 ownerKey = _getOwnerKey();

        vm.startBroadcast(ownerKey);
        Ownable2StepUpgradeable(proxy).transferOwnership(newOwner);
        vm.stopBroadcast();

        console.log("Ownership transfer initiated to:", newOwner);
    }

    function acceptOwnership() public {
        address proxy = address(_getDenylist());
        uint256 newOwnerKey = vm.envUint("NEW_OWNER_KEY");
        require(newOwnerKey != 0, "NEW_OWNER_KEY env var not set or is zero");

        vm.startBroadcast(newOwnerKey);
        Ownable2StepUpgradeable(proxy).acceptOwnership();
        vm.stopBroadcast();

        console.log("Ownership accepted by:", vm.addr(newOwnerKey));
    }

    // ============ Internal Helpers ============

    function _getDenylist() internal view returns (Denylist) {
        address proxy = vm.envAddress("DENYLIST_PROXY");
        require(proxy != address(0), "DENYLIST_PROXY env var not set or is zero");
        require(proxy.code.length > 0, "DENYLIST_PROXY has no code");
        return Denylist(proxy);
    }

    function _getOwnerKey() internal view returns (uint256) {
        uint256 ownerKey = vm.envUint("OWNER_KEY");
        require(ownerKey != 0, "OWNER_KEY env var not set or is zero");
        return ownerKey;
    }

    function _getDenylisterKey() internal view returns (uint256) {
        uint256 denylisterKey = vm.envUint("DENYLISTER_KEY");
        require(denylisterKey != 0, "DENYLISTER_KEY env var not set or is zero");
        return denylisterKey;
    }

    function _rejectProtectedAddresses(Denylist _denylist, address[] calldata accounts) internal view {
        address proxy = address(_denylist);
        address proxyAdmin = AdminUpgradeableProxy(payable(proxy)).admin();
        address owner = _denylist.owner();

        for (uint256 i = 0; i < accounts.length; i++) {
            address account = accounts[i];
            require(account != proxyAdmin, "cannot denylist proxy admin");
            require(account != owner, "cannot denylist owner");
        }
    }
}
