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
import {MockValidatorRegisterer} from "../mocks/MockValidatorRegisterer.sol";
import {ValidatorRegisterer} from "../../../src/validator-manager/roles/ValidatorRegisterer.sol";

contract ValidatorRegistererTest is Test {
    // Test events
    event ValidatorRegistererAdded(address indexed validatorRegisterer);
    event ValidatorRegistererRemoved(address indexed validatorRegisterer);

    // Test constants
    address owner = address(10);
    address validatorRegisterer = address(20);
    address nonOwner = address(30);
    address anotherRegisterer = address(40);

    MockValidatorRegisterer mockValidatorRegisterer;

    function setUp() public {
        vm.startPrank(owner);
        mockValidatorRegisterer = new MockValidatorRegisterer(owner);

        assertEq(mockValidatorRegisterer.owner(), owner);
        vm.stopPrank();
    }

    // Tests
    function test_InitialState() public view {
        // Initially no validator registerers should be added
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(address(0)));
    }

    function test_AddValidatorRegisterer_Success() public {
        vm.startPrank(owner);

        // Expect the event to be emitted
        vm.expectEmit(true, true, true, true);
        emit ValidatorRegistererAdded(validatorRegisterer);

        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);

        // Verify registerer is now added
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));

        vm.stopPrank();
    }

    function test_AddValidatorRegisterer_OnlyOwner() public {
        // Non-owner should not be able to add validator registerer
        vm.startPrank(nonOwner);
        vm.expectRevert();
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        vm.stopPrank();

        // Verify registerer was not added
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
    }

    function test_AddValidatorRegisterer_AlreadyAdded() public {
        vm.startPrank(owner);

        // Add registerer initially
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));

        // Add the same registerer again (should revert)
        vm.expectRevert(ValidatorRegisterer.ValidatorRegistererAlreadyAdded.selector);
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);

        vm.stopPrank();
    }

    function test_AddValidatorRegisterer_ZeroAddress() public {
        vm.startPrank(owner);

        // Should not be able to add zero address
        vm.expectRevert();
        mockValidatorRegisterer.addValidatorRegisterer(address(0));

        vm.stopPrank();
    }

    function test_RemoveValidatorRegisterer_Success() public {
        vm.startPrank(owner);

        // First add a validator registerer
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));

        // Now remove it
        vm.expectEmit(true, true, true, true);
        emit ValidatorRegistererRemoved(validatorRegisterer);

        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);

        // Verify registerer is no longer added
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));

        vm.stopPrank();
    }

    function test_RemoveValidatorRegisterer_OnlyOwner() public {
        vm.startPrank(owner);
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        vm.stopPrank();

        // Non-owner should not be able to remove validator registerer
        vm.startPrank(nonOwner);
        vm.expectRevert();
        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);
        vm.stopPrank();

        // Verify registerer is still added
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
    }

    function test_RemoveValidatorRegisterer_NonExistent() public {
        vm.startPrank(owner);

        // Should revert when trying to remove non-existent registerer
        vm.expectRevert(ValidatorRegisterer.ValidatorRegistererNotAdded.selector);
        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);

        // Still should not be a registerer
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));

        vm.stopPrank();
    }

    function test_RemoveValidatorRegisterer_ZeroAddress() public {
        vm.startPrank(owner);

        // Should not be able to remove zero address
        vm.expectRevert();
        mockValidatorRegisterer.removeValidatorRegisterer(address(0));

        vm.stopPrank();
    }

    function test_MultipleValidatorRegisterers() public {
        vm.startPrank(owner);

        // Add multiple registerers
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        mockValidatorRegisterer.addValidatorRegisterer(anotherRegisterer);

        // Both should be registerers
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        // Remove one registerer
        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);

        // Only the second one should remain
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        vm.stopPrank();
    }

    function test_IsValidatorRegisterer_View() public {
        // Test the view function with various addresses
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(owner));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(address(0)));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(address(0xdead)));

        // Add a registerer and test again
        vm.startPrank(owner);
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        vm.stopPrank();

        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(owner));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));
    }

    function test_AddValidatorRegisterer_EmitsCorrectEvent() public {
        vm.startPrank(owner);

        // Test that the event is emitted with correct parameters
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistererAdded(validatorRegisterer);

        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);

        vm.stopPrank();
    }

    function test_RemoveValidatorRegisterer_EmitsCorrectEvent() public {
        vm.startPrank(owner);

        // Add first
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);

        // Test that the remove event is emitted with correct parameters
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistererRemoved(validatorRegisterer);

        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);

        vm.stopPrank();
    }

    function test_CompleteWorkflow() public {
        vm.startPrank(owner);

        // Add multiple registerers
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        mockValidatorRegisterer.addValidatorRegisterer(anotherRegisterer);

        // Verify both are added
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        // Remove one
        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        // Re-add the first one
        mockValidatorRegisterer.addValidatorRegisterer(validatorRegisterer);
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertTrue(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        // Remove both
        mockValidatorRegisterer.removeValidatorRegisterer(validatorRegisterer);
        mockValidatorRegisterer.removeValidatorRegisterer(anotherRegisterer);
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(validatorRegisterer));
        assertFalse(mockValidatorRegisterer.isValidatorRegisterer(anotherRegisterer));

        vm.stopPrank();
    }
}
