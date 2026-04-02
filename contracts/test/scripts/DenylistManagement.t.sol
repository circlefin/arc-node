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

import {Test} from "forge-std/Test.sol";
import {Denylist} from "../../src/Denylist.sol";
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";
import {DenylistManagement} from "../../scripts/DenylistManagement.s.sol";

contract DenylistManagementTest is Test {
    DenylistManagement script;
    Denylist denylist;
    address proxyAddr;

    uint256 ownerPk = uint256(keccak256("OWNER_PK"));
    address owner = vm.addr(ownerPk);
    uint256 denylisterPk = uint256(keccak256("DENYLISTER_PK"));
    address denylisterAddr = vm.addr(denylisterPk);
    uint256 newOwnerPk = uint256(keccak256("NEW_OWNER_PK"));
    address newOwner = vm.addr(newOwnerPk);

    uint256 proxyAdminPk = uint256(keccak256("PROXY_ADMIN_PK"));
    address proxyAdminAddr = vm.addr(proxyAdminPk);

    address alice = address(40);
    address bob = address(50);

    function setUp() public {
        // Deploy Denylist behind proxy (distinct proxy admin and owner)
        Denylist impl = new Denylist();
        bytes memory initData = abi.encodeWithSelector(Denylist.initialize.selector, owner);
        AdminUpgradeableProxy proxy = new AdminUpgradeableProxy(address(impl), proxyAdminAddr, initData);
        proxyAddr = address(proxy);
        denylist = Denylist(proxyAddr);

        // Set DENYLIST_PROXY env var for the script
        vm.setEnv("DENYLIST_PROXY", vm.toString(proxyAddr));

        // Add a denylister via direct prank (setup prerequisite)
        vm.prank(owner);
        denylist.addDenylister(denylisterAddr);

        script = new DenylistManagement();
    }

    // ============ Print / View Functions ============

    function test_PrintContractInfo_Succeeds() public view {
        // Should not revert — exercises console.log paths
        script.printContractInfo();
    }

    function test_PrintIsDenylisted_Succeeds() public view {
        script.printIsDenylisted(alice);
    }

    function test_PrintIsDenylister_Succeeds() public view {
        script.printIsDenylister(denylisterAddr);
    }

    function test_PrintBatchDenylistStatus_Succeeds() public view {
        address[] memory accounts = new address[](2);
        accounts[0] = alice;
        accounts[1] = bob;
        script.printBatchDenylistStatus(accounts);
    }

    // ============ Denylister Management ============

    function test_AddDenylister_Succeeds() public {
        vm.setEnv("OWNER_KEY", vm.toString(ownerPk));

        address newDenylister = address(100);
        script.addDenylister(newDenylister);

        assertTrue(denylist.isDenylister(newDenylister));
    }

    function test_RemoveDenylister_Succeeds() public {
        vm.setEnv("OWNER_KEY", vm.toString(ownerPk));

        script.removeDenylister(denylisterAddr);

        assertFalse(denylist.isDenylister(denylisterAddr));
    }

    // ============ Denylist Operations ============

    function test_Denylist_Succeeds() public {
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));

        address[] memory accounts = new address[](2);
        accounts[0] = alice;
        accounts[1] = bob;
        script.denylist(accounts);

        assertTrue(denylist.isDenylisted(alice));
        assertTrue(denylist.isDenylisted(bob));
    }

    function test_Denylist_RevertsForEmptyAccounts() public {
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));

        address[] memory accounts = new address[](0);

        vm.expectRevert("accounts cannot be empty");
        script.denylist(accounts);
    }

    function test_Denylist_RevertsForProxyAdmin() public {
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));

        address[] memory accounts = new address[](1);
        accounts[0] = proxyAdminAddr;

        vm.expectRevert("cannot denylist proxy admin");
        script.denylist(accounts);
    }

    function test_Denylist_RevertsForOwner() public {
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));

        address[] memory accounts = new address[](1);
        accounts[0] = owner;

        vm.expectRevert("cannot denylist owner");
        script.denylist(accounts);
    }

    function test_UnDenylist_Succeeds() public {
        // First denylist alice
        vm.prank(denylisterAddr);
        address[] memory add = new address[](1);
        add[0] = alice;
        denylist.denylist(add);
        assertTrue(denylist.isDenylisted(alice));

        // Un-denylist via script
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));
        address[] memory remove = new address[](1);
        remove[0] = alice;
        script.unDenylist(remove);

        assertFalse(denylist.isDenylisted(alice));
    }

    function test_UnDenylist_RevertsForEmptyAccounts() public {
        vm.setEnv("DENYLISTER_KEY", vm.toString(denylisterPk));

        address[] memory accounts = new address[](0);

        vm.expectRevert("accounts cannot be empty");
        script.unDenylist(accounts);
    }

    // ============ Ownership Transfer ============

    function test_TransferOwnership_Succeeds() public {
        vm.setEnv("OWNER_KEY", vm.toString(ownerPk));

        script.transferOwnership(newOwner);

        assertEq(denylist.owner(), owner);
        assertEq(denylist.pendingOwner(), newOwner);
    }

    function test_AcceptOwnership_Succeeds() public {
        // Initiate transfer first
        vm.prank(owner);
        denylist.transferOwnership(newOwner);

        vm.setEnv("NEW_OWNER_KEY", vm.toString(newOwnerPk));
        script.acceptOwnership();

        assertEq(denylist.owner(), newOwner);
        assertEq(denylist.pendingOwner(), address(0));
    }

    function test_TransferOwnership_RevertsForZeroAddress() public {
        vm.setEnv("OWNER_KEY", vm.toString(ownerPk));

        vm.expectRevert("new owner cannot be zero address");
        script.transferOwnership(address(0));
    }
}
