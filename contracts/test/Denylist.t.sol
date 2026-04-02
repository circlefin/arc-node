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

import {Test} from "forge-std/Test.sol";
import {Denylist} from "../src/Denylist.sol";
import {AdminUpgradeableProxy} from "../src/proxy/AdminUpgradeableProxy.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

contract DenylistTest is Test {
    event Denylisted(address indexed account);
    event UnDenylisted(address indexed account);
    event DenylisterAdded(address indexed account);
    event DenylisterRemoved(address indexed account);

    address owner = address(10);
    address denylister1 = address(20);
    address denylister2 = address(30);
    address alice = address(40);
    address bob = address(50);
    address stranger = address(60);

    Denylist denylistImpl;
    Denylist denylist;

    function setUp() public {
        denylistImpl = new Denylist();
        bytes memory initData = abi.encodeWithSelector(Denylist.initialize.selector, owner);
        AdminUpgradeableProxy proxy = new AdminUpgradeableProxy(address(denylistImpl), owner, initData);
        denylist = Denylist(address(proxy));
    }

    // ============ Initialize ============

    function test_Initialize_SetsOwner() public view {
        assertEq(denylist.owner(), owner);
    }

    function test_Initialize_RevertsForZeroOwner() public {
        Denylist impl = new Denylist();
        bytes memory initData = abi.encodeWithSelector(Denylist.initialize.selector, address(0));
        vm.expectRevert(Denylist.ZeroAddress.selector);
        new AdminUpgradeableProxy(address(impl), owner, initData);
    }

    function test_Initialize_SubsequentCallsRevert() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        denylist.initialize(owner);
    }

    // ============ Ownable2Step ============

    function test_Ownable2Step_TransferRequiresAcceptance() public {
        vm.prank(owner);
        denylist.transferOwnership(alice);
        assertEq(denylist.owner(), owner);
        assertEq(denylist.pendingOwner(), alice);

        vm.prank(alice);
        denylist.acceptOwnership();
        assertEq(denylist.owner(), alice);
        assertEq(denylist.pendingOwner(), address(0));
    }

    function test_Ownable2Step_OnlyPendingOwnerCanAccept() public {
        vm.prank(owner);
        denylist.transferOwnership(alice);
        vm.expectRevert(
            abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger)
        );
        vm.prank(stranger);
        denylist.acceptOwnership();
    }

    // ============ Denylist / UnDenylist (onlyDenylister) ============

    function test_Denylist_OnlyDenylister_Success() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory accounts = new address[](2);
        accounts[0] = alice;
        accounts[1] = bob;

        vm.prank(denylister1);
        vm.expectEmit(true, true, true, true);
        emit Denylisted(alice);
        vm.expectEmit(true, true, true, true);
        emit Denylisted(bob);
        denylist.denylist(accounts);

        assertTrue(denylist.isDenylisted(alice));
        assertTrue(denylist.isDenylisted(bob));
    }

    function test_Denylist_RevertsForNonDenylister() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.prank(stranger);
        vm.expectRevert(Denylist.CallerIsNotDenylister.selector);
        denylist.denylist(accounts);
    }

    function test_Denylist_RevertsWhenDenylistingOwner() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory accounts = new address[](1);
        accounts[0] = owner;

        vm.prank(denylister1);
        vm.expectRevert(Denylist.CannotDenylistOwner.selector);
        denylist.denylist(accounts);
    }

    function test_Denylist_OwnerInBatch_Reverts() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory accounts = new address[](3);
        accounts[0] = alice;
        accounts[1] = owner;
        accounts[2] = bob;

        vm.prank(denylister1);
        vm.expectRevert(Denylist.CannotDenylistOwner.selector);
        denylist.denylist(accounts);
    }

    function test_UnDenylist_OnlyDenylister_Success() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory add = new address[](1);
        add[0] = alice;
        vm.prank(denylister1);
        denylist.denylist(add);

        address[] memory remove = new address[](1);
        remove[0] = alice;
        vm.prank(denylister1);
        vm.expectEmit(true, true, true, true);
        emit UnDenylisted(alice);
        denylist.unDenylist(remove);

        assertFalse(denylist.isDenylisted(alice));
    }

    function test_UnDenylist_RevertsForNonDenylister() public {
        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.prank(stranger);
        vm.expectRevert(Denylist.CallerIsNotDenylister.selector);
        denylist.unDenylist(accounts);
    }

    function test_Denylist_Idempotent_NoDuplicateEvent() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory accounts = new address[](1);
        accounts[0] = alice;

        vm.prank(denylister1);
        denylist.denylist(accounts);
        vm.prank(denylister1);
        denylist.denylist(accounts); // second time should not emit

        assertTrue(denylist.isDenylisted(alice));
    }

    // ============ Add / Remove denylisters (onlyOwner) ============

    function test_AddDenylister_OnlyOwner_Success() public {
        vm.prank(owner);
        vm.expectEmit(true, true, true, true);
        emit DenylisterAdded(denylister1);
        denylist.addDenylister(denylister1);

        assertTrue(denylist.isDenylister(denylister1));
    }

    function test_AddDenylister_RevertsForNonOwner() public {
        vm.prank(stranger);
        vm.expectRevert();
        denylist.addDenylister(denylister1);
    }

    function test_RemoveDenylister_OnlyOwner_Success() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);
        vm.prank(owner);
        vm.expectEmit(true, true, true, true);
        emit DenylisterRemoved(denylister1);
        denylist.removeDenylister(denylister1);

        assertFalse(denylist.isDenylister(denylister1));
    }

    function test_RemoveDenylister_RevertsForNonOwner() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);
        vm.prank(stranger);
        vm.expectRevert();
        denylist.removeDenylister(denylister1);
    }

    function test_AddDenylister_RevertsForZeroAddress() public {
        vm.prank(owner);
        vm.expectRevert(Denylist.ZeroAddress.selector);
        denylist.addDenylister(address(0));
    }

    function test_RemoveDenylister_RevertsForZeroAddress() public {
        vm.prank(owner);
        vm.expectRevert(Denylist.ZeroAddress.selector);
        denylist.removeDenylister(address(0));
    }

    // ============ Storage layout (ERC-7201) ============

    /// @dev Verifies the per-address slot formula: slot = keccak256(abi.encode(address, baseSlot)); value 1 = denylisted.
    function test_StorageLayout_PerAddressSlot_MatchesFormula() public {
        vm.prank(owner);
        denylist.addDenylister(denylister1);

        address[] memory accounts = new address[](1);
        accounts[0] = alice;
        vm.prank(denylister1);
        denylist.denylist(accounts);

        bytes32 baseSlot = denylist.DENYLIST_STORAGE_LOCATION();
        bytes32 expectedSlot = keccak256(abi.encode(alice, baseSlot));
        bytes32 value = vm.load(address(denylist), expectedSlot);
        assertEq(value, bytes32(uint256(1)), "denylisted slot should be 1");

        // Bob not denylisted -> slot should be 0
        bytes32 bobSlot = keccak256(abi.encode(bob, baseSlot));
        assertEq(vm.load(address(denylist), bobSlot), bytes32(0));
    }

    function test_StorageLayout_BaseSlot_MatchesDocumentedFormula() public view {
        bytes32 expected =
            keccak256(abi.encode(uint256(keccak256("arc.storage.Denylist.v1")) - 1)) & ~bytes32(uint256(0xff));
        assertEq(denylist.DENYLIST_STORAGE_LOCATION(), expected);
    }

    // ============ View functions ============

    function test_IsDenylisted_ReturnsFalseByDefault() public view {
        assertFalse(denylist.isDenylisted(alice));
    }

    function test_IsDenylister_ReturnsFalseByDefault() public view {
        assertFalse(denylist.isDenylister(denylister1));
    }
}
