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
import {MockController} from "../mocks/MockController.sol";
import {Controller} from "../../../src/validator-manager/roles/Controller.sol";

contract ControllerTest is Test {
    // Test events
    event ControllerConfigured(address indexed controller, uint256 indexed registrationId, uint64 votingPowerLimit);
    event ControllerRemoved(address indexed controller);
    event VotingPowerLimitUpdated(address indexed controller, uint64 newVotingPowerLimit);

    // Test constants
    address owner = address(10);
    address controllerAddr = address(20);
    address nonOwner = address(30);
    address anotherController = address(40);
    uint64 defaultVotingPowerLimit = 500;

    MockController mockController;

    function setUp() public {
        vm.startPrank(owner);
        mockController = new MockController(owner);

        assertEq(mockController.owner(), owner);
        vm.stopPrank();
    }

    // Tests
    function test_InitialState() public view {
        // Initially no controllers should be configured
        assertFalse(mockController.isController(controllerAddr));
        assertFalse(mockController.isController(anotherController));
        assertFalse(mockController.isController(address(0)));

        // Initially all controllers should have registration ID 0
        assertEq(mockController.getRegistrationId(controllerAddr), 0);
        assertEq(mockController.getRegistrationId(anotherController), 0);
        assertEq(mockController.getRegistrationId(address(0)), 0);
    }

    function test_ConfigureController_Success() public {
        vm.startPrank(owner);

        // Expect the event to be emitted with the provided registration ID
        vm.expectEmit(true, true, true, true);
        emit ControllerConfigured(controllerAddr, 1, defaultVotingPowerLimit);

        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);

        // Verify controller is now configured
        assertTrue(mockController.isController(controllerAddr));

        // Verify controller has correct registration ID
        assertEq(mockController.getRegistrationId(controllerAddr), 1);
        // Verify controller has correct voting power limit
        assertEq(mockController.getVotingPowerLimit(controllerAddr), defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_UpdateVotingPowerLimit_Success() public {
        vm.startPrank(owner);
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);

        uint64 newVotingPowerLimit = 777;
        vm.expectEmit(true, false, false, true);
        emit VotingPowerLimitUpdated(controllerAddr, newVotingPowerLimit);

        mockController.updateVotingPowerLimit(controllerAddr, newVotingPowerLimit);
        assertEq(mockController.getVotingPowerLimit(controllerAddr), newVotingPowerLimit);
        vm.stopPrank();
    }

    function test_UpdateVotingPowerLimit_OnlyOwner() public {
        vm.prank(owner);
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);

        vm.startPrank(nonOwner);
        vm.expectRevert();
        mockController.updateVotingPowerLimit(controllerAddr, 9);
        vm.stopPrank();
    }

    function test_UpdateVotingPowerLimit_ControllerNotConfigured() public {
        vm.startPrank(owner);
        vm.expectRevert(Controller.ControllerNotConfigured.selector);
        mockController.updateVotingPowerLimit(controllerAddr, 9);
        vm.stopPrank();
    }

    function test_ConfigureController_OnlyOwner() public {
        // Non-owner should not be able to configure controller
        vm.startPrank(nonOwner);
        vm.expectRevert();
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        vm.stopPrank();

        // Verify controller was not configured
        assertFalse(mockController.isController(controllerAddr));

        // Verify registration ID remains 0
        assertEq(mockController.getRegistrationId(controllerAddr), 0);
    }

    function test_ConfigureController_AlreadyConfigured() public {
        vm.startPrank(owner);

        // Configure controller initially
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        assertTrue(mockController.isController(controllerAddr));

        // Configure the same controller again (should revert)
        vm.expectRevert(Controller.ControllerAlreadyConfigured.selector);
        mockController.configureController(controllerAddr, 2, defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_ConfigureController_MultipleRegistrationIDs() public {
        vm.startPrank(owner);

        // Configure multiple controllers with different registration IDs
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        mockController.configureController(anotherController, 2, defaultVotingPowerLimit);

        assertTrue(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));

        // Verify registration ID is correct
        assertEq(mockController.getRegistrationId(controllerAddr), 1);
        assertEq(mockController.getRegistrationId(anotherController), 2);

        vm.stopPrank();
    }

    function test_ConfigureController_ZeroAddress() public {
        vm.startPrank(owner);

        // Should not be able to configure zero address (edge case)
        vm.expectRevert();
        mockController.configureController(address(0), 1, defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_ConfigureController_ZeroRegistrationID() public {
        vm.startPrank(owner);

        vm.expectRevert(Controller.RegistrationIdIsZero.selector);
        mockController.configureController(address(1), 0, defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_RemoveController_Success() public {
        vm.startPrank(owner);

        // First configure a controller
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        assertTrue(mockController.isController(controllerAddr));

        // Now remove it
        vm.expectEmit(true, true, true, true);
        emit ControllerRemoved(controllerAddr);

        mockController.removeController(controllerAddr);

        // Verify controller is no longer configured
        assertFalse(mockController.isController(controllerAddr));
        // Registration ID and voting power limit should be cleared
        assertEq(mockController.getRegistrationId(controllerAddr), 0);
        assertEq(mockController.getVotingPowerLimit(controllerAddr), 0);

        vm.stopPrank();
    }

    function test_RemoveController_OnlyOwner() public {
        vm.startPrank(owner);
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        vm.stopPrank();

        // Non-owner should not be able to remove controller
        vm.startPrank(nonOwner);
        vm.expectRevert();
        mockController.removeController(controllerAddr);
        vm.stopPrank();

        // Verify controller is still configured
        assertTrue(mockController.isController(controllerAddr));
    }

    function test_RemoveController_ZeroAddress() public {
        vm.startPrank(owner);

        vm.expectRevert();
        mockController.removeController(address(0));

        vm.stopPrank();
    }

    function test_RemoveController_NonExistent() public {
        vm.startPrank(owner);

        // Should revert when trying to remove non-existent controller
        vm.expectRevert(Controller.ControllerNotConfigured.selector);
        mockController.removeController(controllerAddr);

        // Still should not be a controller
        assertFalse(mockController.isController(controllerAddr));

        vm.stopPrank();
    }

    function test_MultipleControllers() public {
        vm.startPrank(owner);

        // Configure multiple controllers with different registration IDs
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        mockController.configureController(anotherController, 2, defaultVotingPowerLimit);

        // Both should be controllers
        assertTrue(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));
        assertEq(mockController.getVotingPowerLimit(controllerAddr), defaultVotingPowerLimit);
        assertEq(mockController.getVotingPowerLimit(anotherController), defaultVotingPowerLimit);

        // Remove one controller
        mockController.removeController(controllerAddr);

        // Only the second one should remain
        assertFalse(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));
        assertEq(mockController.getVotingPowerLimit(controllerAddr), 0);
        assertEq(mockController.getVotingPowerLimit(anotherController), defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_IsController_View() public {
        // Test the view function with various addresses
        assertFalse(mockController.isController(controllerAddr));
        assertFalse(mockController.isController(owner));
        assertFalse(mockController.isController(address(0)));
        assertFalse(mockController.isController(address(0xdead)));

        // Configure a controller and test again
        vm.startPrank(owner);
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        vm.stopPrank();

        assertTrue(mockController.isController(controllerAddr));
        assertFalse(mockController.isController(owner));
        assertFalse(mockController.isController(anotherController));
    }

    function test_ConfigureController_EmitsCorrectEvent() public {
        vm.startPrank(owner);

        // Test that the event is emitted with correct parameters
        vm.expectEmit(true, false, false, true);
        emit ControllerConfigured(controllerAddr, 1, defaultVotingPowerLimit);

        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);

        vm.stopPrank();
    }

    function test_RemoveController_EmitsCorrectEvent() public {
        vm.startPrank(owner);

        // Configure first
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);

        // Test that the remove event is emitted with correct parameters
        vm.expectEmit(true, false, false, true);
        emit ControllerRemoved(controllerAddr);

        mockController.removeController(controllerAddr);

        vm.stopPrank();
    }

    function test_CompleteWorkflow() public {
        vm.startPrank(owner);

        // Configure multiple controllers with different registration IDs
        mockController.configureController(controllerAddr, 1, defaultVotingPowerLimit);
        mockController.configureController(anotherController, 2, defaultVotingPowerLimit);

        // Verify both are configured
        assertTrue(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));
        assertEq(mockController.getVotingPowerLimit(controllerAddr), defaultVotingPowerLimit);
        assertEq(mockController.getVotingPowerLimit(anotherController), defaultVotingPowerLimit);

        // Verify registration IDs are correct
        assertEq(mockController.getRegistrationId(controllerAddr), 1);
        assertEq(mockController.getRegistrationId(anotherController), 2);

        // Remove one
        mockController.removeController(controllerAddr);
        assertFalse(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));

        // Verify registration ID is cleared after removal
        assertEq(mockController.getRegistrationId(controllerAddr), 0);
        assertEq(mockController.getRegistrationId(anotherController), 2);
        assertEq(mockController.getVotingPowerLimit(controllerAddr), 0);
        assertEq(mockController.getVotingPowerLimit(anotherController), defaultVotingPowerLimit);

        // Re-configure the first one with a different registration ID
        mockController.configureController(controllerAddr, 3, defaultVotingPowerLimit);
        assertTrue(mockController.isController(controllerAddr));
        assertTrue(mockController.isController(anotherController));
        assertEq(mockController.getVotingPowerLimit(controllerAddr), defaultVotingPowerLimit);
        assertEq(mockController.getVotingPowerLimit(anotherController), defaultVotingPowerLimit);

        // Verify the new registration ID
        assertEq(mockController.getRegistrationId(controllerAddr), 3);
        assertEq(mockController.getRegistrationId(anotherController), 2);

        // Remove both
        mockController.removeController(controllerAddr);
        mockController.removeController(anotherController);
        assertFalse(mockController.isController(controllerAddr));
        assertFalse(mockController.isController(anotherController));
        assertEq(mockController.getVotingPowerLimit(controllerAddr), 0);
        assertEq(mockController.getVotingPowerLimit(anotherController), 0);

        vm.stopPrank();
    }
}
