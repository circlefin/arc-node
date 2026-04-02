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

import {PermissionedValidatorManager} from "../../src/validator-manager/PermissionedValidatorManager.sol";
import {ValidatorRegistry} from "../../src/validator-manager/ValidatorRegistry.sol";
import {Validator, ValidatorStatus} from "../../src/validator-manager/interfaces/IValidatorRegistry.sol";
import {Controller} from "../../src/validator-manager/roles/Controller.sol";
import {Pausable} from "../../src/common/roles/Pausable.sol";
import {TestUtils} from "./TestUtils.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";

contract PermissionedValidatorManagerTest is TestUtils {
    // Roles events
    event ValidatorRegistererAdded(address indexed validatorRegisterer);
    event ValidatorRegistererRemoved(address indexed validatorRegisterer);
    event ControllerConfigured(address indexed controller, uint256 indexed registrationId, uint64 indexed votingPowerLimit);
    event ControllerRemoved(address indexed controller);
    event RegistryOwnerTransferStarted(address indexed newOwner);
    event RegistryOwnerTransferCompleted();
    event Pause();
    event Unpause();

    // Test constants
    address owner = address(10);
    address validatorRegisterer = address(20);
    address controller1 = address(30);
    address controller2 = address(40);
    uint64 defaultVotingPower;
    uint64 controllerVotingPowerLimit = 500;
    address pauser = address(50);
    bytes validator1PublicKey;
    bytes validator2PublicKey;
    address nonOwner = address(70);
    address notRegisterer = address(80);
    address notController = address(90);

    PermissionedValidatorManager permissionedValidatorManager;
    ValidatorRegistry registry;

    function setUp() public {
        // Initialize validator public keys
        validator1PublicKey = generateEd25519PublicKey(50);
        validator2PublicKey = generateEd25519PublicKey(60);

        vm.startPrank(owner);

        // Deploy registry (no constructor parameters for upgradeable pattern)
        registry = new ValidatorRegistry();

        // Set up ownership and initial state manually (simulating genesis initialization)
        _setRegistryOwner(registry, owner);

        // Deploy PermissionedValidatorManager
        permissionedValidatorManager = new PermissionedValidatorManager(registry);

        // Set owner manually using storage slot (simulating genesis initialization)
        _setPvmOwner(permissionedValidatorManager, owner);

        // Transfer ownership of the registry to the PermissionedValidatorManager
        registry.transferOwnership(address(permissionedValidatorManager));

        vm.stopPrank();

        // Accept ownership (required for Ownable2StepUpgradeable)
        vm.prank(address(permissionedValidatorManager));
        registry.acceptOwnership();

        assertEq(permissionedValidatorManager.owner(), owner);
        assertEq(registry.owner(), address(permissionedValidatorManager));
        defaultVotingPower = permissionedValidatorManager.DEFAULT_VOTING_POWER();
        assertEq(defaultVotingPower, 0);
    }

    // ============ Initialize Tests ============

    function test_Initialize_ThroughProxy_Success() public {
        // Deploy implementation
        PermissionedValidatorManager impl = new PermissionedValidatorManager(registry);
        address expectedOwner = address(999);
        address expectedPauser = address(777);
        address proxyAdmin = address(888);

        // Deploy proxy with initialize call
        bytes memory initData = abi.encodeWithSignature("initialize(address,address)", expectedOwner, expectedPauser);
        AdminUpgradeableProxy proxy = new AdminUpgradeableProxy(address(impl), proxyAdmin, initData);
        PermissionedValidatorManager newPvm = PermissionedValidatorManager(address(proxy));

        // Verify owner is set
        assertEq(newPvm.owner(), expectedOwner);
        assertEq(newPvm.pauser(), expectedPauser);

        // Verify pending owner is not set
        assertEq(newPvm.pendingOwner(), address(0));

        // Verify the new owner can perform owner actions
        vm.prank(expectedOwner);
        newPvm.addValidatorRegisterer(address(123));
        assertTrue(newPvm.isValidatorRegisterer(address(123)));
    }

    function test_Initialize_ThroughProxy_CanOnlyBeCalledOnce() public {
        // Deploy implementation and proxy
        PermissionedValidatorManager impl = new PermissionedValidatorManager(registry);
        address newOwner = address(100);
        address newPauser = address(101);
        address proxyAdmin = address(888);

        bytes memory initData = abi.encodeWithSignature("initialize(address,address)", newOwner, newPauser);
        AdminUpgradeableProxy proxy = new AdminUpgradeableProxy(address(impl), proxyAdmin, initData);
        PermissionedValidatorManager newPvm = PermissionedValidatorManager(address(proxy));

        // Try to initialize again - should revert
        vm.expectRevert();
        newPvm.initialize(address(200), address(201));
    }

    function test_Initialize_CannotBeCalledOnImplementation() public {
        // Deploy implementation - _disableInitializers() is called in constructor
        PermissionedValidatorManager impl = new PermissionedValidatorManager(registry);

        // Should revert because _disableInitializers() was called in constructor
        vm.expectRevert();
        impl.initialize(owner, pauser);
    }

    function test_Initialize_ThroughProxy_WithZeroAddress() public {
        // Deploy implementation
        PermissionedValidatorManager impl = new PermissionedValidatorManager(registry);
        address proxyAdmin = address(888);

        // Initialize with zero owner through proxy - should revert during initialization
        bytes memory initData = abi.encodeWithSignature("initialize(address,address)", address(0), owner);
        vm.expectRevert(abi.encodeWithSelector(PermissionedValidatorManager.ZeroOwnerAddress.selector));
        new AdminUpgradeableProxy(address(impl), proxyAdmin, initData);
    }

    function test_Initialize_ThroughProxy_WithZeroPauser() public {
        PermissionedValidatorManager impl = new PermissionedValidatorManager(registry);
        address proxyAdmin = address(888);

        bytes memory initData = abi.encodeWithSignature("initialize(address,address)", owner, address(0));
        vm.expectRevert(abi.encodeWithSelector(Pausable.ZeroPauserAddress.selector));
        new AdminUpgradeableProxy(address(impl), proxyAdmin, initData);
    }

    // Tests
    function test_InitialState() public view {
        // Initially no validator registerers or controllers should be configured
        assertFalse(permissionedValidatorManager.isValidatorRegisterer(validatorRegisterer));
        assertFalse(permissionedValidatorManager.isController(controller1));
        assertFalse(permissionedValidatorManager.isController(controller2));
    }

    // ============ Validator Management Tests ============

    function test_RegisterValidator_Success() public {
        vm.startPrank(owner);

        // First add a validator registerer
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);

        vm.stopPrank();
        vm.startPrank(validatorRegisterer);

        bytes memory publicKey = validator1PublicKey;

        // Expect ValidatorRegistered event to be emitted
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(1, defaultVotingPower, publicKey);

        uint256 registrationId = permissionedValidatorManager.registerValidator(publicKey);

        assertEq(registrationId, 1);
        Validator memory v = registry.getValidator(registrationId);
        assertEq(uint8(v.status), uint8(ValidatorStatus.Registered));
        assertEq(v.votingPower, defaultVotingPower);
        vm.stopPrank();
    }

    function test_RegisterValidator_OnlyValidatorRegisterer() public {
        vm.startPrank(notRegisterer);

        bytes memory publicKey = validator1PublicKey;

        vm.expectRevert();
        permissionedValidatorManager.registerValidator(publicKey);

        vm.stopPrank();
    }


    function test_ActivateValidator_Success() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);

        vm.stopPrank();

        // First register a validator with registration ID 1
        vm.startPrank(validatorRegisterer);
        bytes memory publicKey = validator1PublicKey;

        uint256 registrationId = permissionedValidatorManager.registerValidator(publicKey);
        assertEq(registrationId, 1);
        vm.stopPrank();

        // Now controller can activate their assigned validator
        vm.startPrank(controller1);

        // Expect ValidatorActivated event to be emitted
        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(1, defaultVotingPower);

        permissionedValidatorManager.activateValidator();
        vm.stopPrank();
    }

    function test_ActivateValidator_OnlyController() public {
        vm.startPrank(notController);

        vm.expectRevert(Controller.CallerIsNotController.selector);
        permissionedValidatorManager.activateValidator();

        vm.stopPrank();
    }

    function test_RemoveValidator_Success() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);

        vm.stopPrank();

        // First register a validator with registration ID 1
        vm.startPrank(validatorRegisterer);
        bytes memory publicKey = validator1PublicKey;

        uint256 registrationId = permissionedValidatorManager.registerValidator(publicKey);
        assertEq(registrationId, 1);
        vm.stopPrank();

        // Now controller can remove their assigned validator
        vm.startPrank(controller1);

        // Expect ValidatorRemoved event to be emitted
        vm.expectEmit(true, true, false, true);
        emit ValidatorRemoved(1, defaultVotingPower);

        permissionedValidatorManager.removeValidator();
        vm.stopPrank();
    }

    function test_RemoveValidator_OnlyController() public {
        vm.startPrank(notController);

        vm.expectRevert(Controller.CallerIsNotController.selector);
        permissionedValidatorManager.removeValidator();

        vm.stopPrank();
    }

    function test_UpdateValidatorVotingPower_Success() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);

        vm.stopPrank();

        // First register a validator with registration ID 1
        vm.startPrank(validatorRegisterer);
        bytes memory publicKey = validator1PublicKey;

        uint256 registrationId = permissionedValidatorManager.registerValidator(publicKey);
        assertEq(registrationId, 1);
        vm.stopPrank();

        // Now controller can update voting power for their assigned validator
        vm.startPrank(controller1);
        uint64 newVotingPower = 200;

        // Expect ValidatorVotingPowerUpdated event to be emitted
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(1, defaultVotingPower, newVotingPower);

        permissionedValidatorManager.updateValidatorVotingPower(newVotingPower);
        vm.stopPrank();
    }

    function test_UpdateValidatorVotingPower_RevertsAboveLimit() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller with a tight limit
        uint64 votingPowerLimit = 150;
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, votingPowerLimit);

        vm.stopPrank();

        // Register validator
        vm.startPrank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);
        vm.stopPrank();

        // Controller attempts to exceed limit
        vm.startPrank(controller1);
        vm.expectRevert(abi.encodeWithSelector(PermissionedValidatorManager.VotingPowerExceedsLimit.selector, votingPowerLimit));
        permissionedValidatorManager.updateValidatorVotingPower(votingPowerLimit + 1);
        vm.stopPrank();
    }

    function test_UpdateValidatorVotingPower_OnlyController() public {
        vm.startPrank(notController);

        uint64 newVotingPower = 200;

        vm.expectRevert(Controller.CallerIsNotController.selector);
        permissionedValidatorManager.updateValidatorVotingPower(newVotingPower);

        vm.stopPrank();
    }

    function test_UpdateVotingPower_BeforeActivation() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);

        vm.stopPrank();

        // Register validator with default (zero) voting power
        vm.startPrank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);
        vm.stopPrank();

        // Controller updates voting power while validator is still Registered
        uint64 newVotingPower = 123;
        vm.startPrank(controller1);
        permissionedValidatorManager.updateValidatorVotingPower(newVotingPower);

        // Validator should remain in Registered status with updated power
        Validator memory validator = permissionedValidatorManager.getValidator(controller1);
        assertEq(uint8(validator.status), uint8(ValidatorStatus.Registered));
        assertEq(validator.votingPower, newVotingPower);

        vm.stopPrank();

        // Validator set should still be empty (not active yet)
        Validator[] memory activeSet = registry.getActiveValidatorSet();
        assertEq(activeSet.length, 0);
    }

    function test_UpdateValidatorVotingPower_WithZeroLimit_OnlyAllowsZero() public {
        vm.startPrank(owner);

        // Setup controller with zero limit
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, 0);

        vm.stopPrank();

        // Register validator
        vm.startPrank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);
        vm.stopPrank();

        // Attempt to set greater voting power should revert
        vm.prank(controller1);
        vm.expectRevert(abi.encodeWithSelector(PermissionedValidatorManager.VotingPowerExceedsLimit.selector, 0));
        permissionedValidatorManager.updateValidatorVotingPower(1);

        // increase the voting power limit
        vm.prank(owner);
        permissionedValidatorManager.updateVotingPowerLimit(controller1, 1);

        // should succeed now
        vm.startPrank(controller1);
        permissionedValidatorManager.updateValidatorVotingPower(1);

        // Setting zero should succeed as well
        permissionedValidatorManager.updateValidatorVotingPower(0);
        vm.stopPrank();
    }


    function test_UpdateVotingPowerLimit_OnlyOwner() public {
        vm.startPrank(owner);
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);
        vm.stopPrank();

        vm.startPrank(controller1);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, controller1));
        permissionedValidatorManager.updateVotingPowerLimit(controller1, 999);
        vm.stopPrank();
    }

    function test_UpdateControllerVotingPowerLimit_CurrentPowerAboveNewLimit_Succeeds() public {
        vm.startPrank(owner);
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, 200);
        vm.stopPrank();

        // Register and set voting power under initial limit
        vm.startPrank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);
        vm.stopPrank();

        vm.startPrank(controller1);
        permissionedValidatorManager.updateValidatorVotingPower(150);
        vm.stopPrank();

        // Lower limit below current power; should NOT revert
        vm.prank(owner);
        permissionedValidatorManager.updateVotingPowerLimit(controller1, 100);
        uint64 votingPowerLimit = permissionedValidatorManager.getVotingPowerLimit(controller1);
        assertEq(votingPowerLimit, 100);

        // Attempt to keep power above new limit reverts
        vm.startPrank(controller1);
        vm.expectRevert(abi.encodeWithSelector(PermissionedValidatorManager.VotingPowerExceedsLimit.selector, 100));
        permissionedValidatorManager.updateValidatorVotingPower(120);
        vm.stopPrank();

        // Lowering within limit succeeds
        vm.prank(controller1);
        permissionedValidatorManager.updateValidatorVotingPower(90);

        uint64 newVotingPower = permissionedValidatorManager.getValidator(controller1).votingPower;
        assertEq(newVotingPower, 90);
    }

    // ============ Complete Validator Workflow Tests ============
    function test_CompleteValidatorWorkflow() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controllers
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);
        permissionedValidatorManager.configureController(controller2, 2, controllerVotingPowerLimit);

        vm.stopPrank();

        // Step 1: Register validators
        vm.startPrank(validatorRegisterer);
        bytes memory publicKey = validator1PublicKey;
        bytes memory secondaryPublicKey = validator2PublicKey;

        // Expect ValidatorRegistered event for controller1 validator
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(1, defaultVotingPower, publicKey);

        uint256 registrationId = permissionedValidatorManager.registerValidator(publicKey);
        assertEq(registrationId, 1);

        // Register a second validator so removing controller1 validator is not the final active validator
        uint256 secondaryRegistrationId = permissionedValidatorManager.registerValidator(secondaryPublicKey);
        assertEq(secondaryRegistrationId, 2);
        vm.stopPrank();

        // Verify validator info after registration (Registered status)
        vm.startPrank(controller1);
        Validator memory validatorAfterRegistration = permissionedValidatorManager.getValidator(controller1);
        assertEq(uint8(validatorAfterRegistration.status), uint8(ValidatorStatus.Registered));
        assertEq(validatorAfterRegistration.publicKey, publicKey);
        assertEq(validatorAfterRegistration.votingPower, defaultVotingPower);
        vm.stopPrank();

        // Step 2: Activate validator (by controller)
        vm.startPrank(controller1);

        // Expect ValidatorActivated event
        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(1, defaultVotingPower);

        permissionedValidatorManager.activateValidator();

        // Verify validator info after activation (Active status)
        Validator memory validatorAfterActivation = permissionedValidatorManager.getValidator(controller1);
        assertEq(uint8(validatorAfterActivation.status), uint8(ValidatorStatus.Active));
        assertEq(validatorAfterActivation.publicKey, publicKey);
        assertEq(validatorAfterActivation.votingPower, defaultVotingPower);

        vm.stopPrank();

        // Step 3: Update voting power (by controller)
        vm.startPrank(controller1);
        uint64 newVotingPower = 200;

        // Expect ValidatorVotingPowerUpdated event
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(1, defaultVotingPower, newVotingPower);

        permissionedValidatorManager.updateValidatorVotingPower(newVotingPower);

        // Verify validator info after voting power update
        Validator memory validatorAfterUpdate = permissionedValidatorManager.getValidator(controller1);
        assertEq(uint8(validatorAfterUpdate.status), uint8(ValidatorStatus.Active));
        assertEq(validatorAfterUpdate.publicKey, publicKey);
        assertEq(validatorAfterUpdate.votingPower, newVotingPower);

        vm.stopPrank();

        // Activate and power-up controller2 validator so controller1 removal remains valid.
        vm.startPrank(controller2);
        permissionedValidatorManager.activateValidator();
        permissionedValidatorManager.updateValidatorVotingPower(100);
        vm.stopPrank();

        // Step 4: Remove validator (by controller)
        vm.startPrank(controller1);

        // Expect ValidatorRemoved event
        vm.expectEmit(true, true, false, true);
        emit ValidatorRemoved(1, newVotingPower);

        permissionedValidatorManager.removeValidator();

        // Verify validator was removed
        Validator memory removedValidator = permissionedValidatorManager.getValidator(controller1);
        assertEq(uint8(removedValidator.status), uint8(ValidatorStatus.Unknown));

        vm.stopPrank();
    }

    function test_RemoveValidator_WhenRemovingLastActiveValidator_ShouldRevert() public {
        vm.startPrank(owner);

        // Setup: Add validator registerer and configure controller
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);

        vm.stopPrank();

        // Register validator
        vm.startPrank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);
        vm.stopPrank();

        // Activate validator and give it non-zero voting power
        vm.startPrank(controller1);
        permissionedValidatorManager.activateValidator();
        permissionedValidatorManager.updateValidatorVotingPower(200);

        // Removing the only active validator with positive voting power should fail
        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        permissionedValidatorManager.removeValidator();

        // Zeroing voting power for the only active validator with positive voting power should also fail
        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        permissionedValidatorManager.updateValidatorVotingPower(0);
        vm.stopPrank();
    }

    function test_MultipleControllersAndRegisterers() public {
        address controller3 = makeAddr("controller3");

        vm.startPrank(owner);

        // Setup multiple roles
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit); // controller1 manages registrationId 1
        permissionedValidatorManager.configureController(controller2, 2, controllerVotingPowerLimit); // controller2 manages registrationId 2
        permissionedValidatorManager.configureController(controller3, 3, controllerVotingPowerLimit); // controller3 manages registrationId 3

        // Verify setup
        assertTrue(permissionedValidatorManager.isValidatorRegisterer(validatorRegisterer));
        assertTrue(permissionedValidatorManager.isController(controller1));
        assertTrue(permissionedValidatorManager.isController(controller2));
        assertTrue(permissionedValidatorManager.isController(controller3));

        vm.stopPrank();

        // Register multiple validators (sequential IDs 1, 2, and 3)
        vm.startPrank(validatorRegisterer);
        bytes memory publicKey1 = validator1PublicKey;
        bytes memory publicKey2 = validator2PublicKey;
        bytes memory publicKey3 = generateEd25519PublicKey(70);

        // Expect ValidatorRegistered events
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(1, defaultVotingPower, publicKey1);

        uint256 registrationId1 = permissionedValidatorManager.registerValidator(publicKey1);

        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(2, defaultVotingPower, publicKey2);

        uint256 registrationId2 = permissionedValidatorManager.registerValidator(publicKey2);

        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(3, defaultVotingPower, publicKey3);

        uint256 registrationId3 = permissionedValidatorManager.registerValidator(publicKey3);

        assertEq(registrationId1, 1);
        assertEq(registrationId2, 2);
        assertEq(registrationId3, 3);
        vm.stopPrank();

        // TEST: Verify controller1 can ONLY manage validator with registrationId 1 (access control)
        vm.startPrank(controller1);

        // Expect ValidatorActivated event for validator1 (registrationId 1), NOT validator2
        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(1, defaultVotingPower);

        permissionedValidatorManager.activateValidator();

        // Expect ValidatorVotingPowerUpdated event for validator1, NOT validator2
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(1, defaultVotingPower, 120);

        permissionedValidatorManager.updateValidatorVotingPower(120);
        vm.stopPrank();

        // TEST: Verify controller2 can ONLY manage validator with registrationId 2 (access control)
        vm.startPrank(controller2);

        // Expect ValidatorActivated event for validator2 (registrationId 2), NOT validator1
        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(2, defaultVotingPower);

        permissionedValidatorManager.activateValidator();

        // Expect ValidatorVotingPowerUpdated event for validator2, NOT validator1
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(2, defaultVotingPower, 180);

        permissionedValidatorManager.updateValidatorVotingPower(180);
        vm.stopPrank();

        // Activate and power-up validator3 so controller2 removal remains valid later
        vm.startPrank(controller3);

        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(3, defaultVotingPower);
        permissionedValidatorManager.activateValidator();

        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(3, defaultVotingPower, 210);
        permissionedValidatorManager.updateValidatorVotingPower(210);
        vm.stopPrank();

        // FINAL VERIFICATION: Each controller can only remove their own validator
        vm.startPrank(controller1);

        vm.expectEmit(true, true, false, true);
        emit ValidatorRemoved(1, 120); // controller1 removes validator1, NOT validator2

        permissionedValidatorManager.removeValidator();
        vm.stopPrank();

        // At this point, validator1 is removed but validator2 should still be manageable by controller2
        vm.startPrank(controller2);

        vm.expectEmit(true, true, false, true);
        emit ValidatorRemoved(2, 180); // controller2 removes validator2

        permissionedValidatorManager.removeValidator();

        // Validator2 should be removed successfully
        Validator memory validator2AfterRemoval = permissionedValidatorManager.getValidator(controller2);
        assertEq(uint8(validator2AfterRemoval.status), uint8(ValidatorStatus.Unknown));
        vm.stopPrank();
    }

    function test_TransferRegistryOwnership_OnlyOwner(address _randomCaller, address _randomNewOwner) public {
        vm.assume(_randomNewOwner != address(0));
        vm.assume(_randomCaller != owner);

        vm.startPrank(_randomCaller);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, _randomCaller));
        permissionedValidatorManager.transferRegistryOwner(_randomNewOwner);
        vm.stopPrank();
    }

    function test_TransferRegistryOwnership_RevertsZeroAddress() public {
        vm.startPrank(owner);
        vm.expectRevert(PermissionedValidatorManager.ZeroOwnerAddress.selector);
        permissionedValidatorManager.transferRegistryOwner(address(0));
        vm.stopPrank();
    }

    function test_TransferRegistryOwnership_Succeeds(address _newOwner) public {
        vm.assume(_newOwner != address(0));

        vm.startPrank(owner);

        // Check event: (topic1: newOwner) (topic2: unused) (topic3: unused) (data: empty)
        vm.expectEmit(true, false, false, true);
        emit RegistryOwnerTransferStarted(_newOwner);
        permissionedValidatorManager.transferRegistryOwner(_newOwner);
        vm.stopPrank();

        // Check side effect
        assertEq(registry.pendingOwner(), _newOwner);
    }

    // ============ AcceptRegistryOwnership Tests ============

    function test_AcceptRegistryOwnership_OnlyOwner(address _randomCaller) public {
        vm.assume(_randomCaller != owner);

        // First transfer ownership to PVM
        vm.prank(address(permissionedValidatorManager));
        registry.transferOwnership(address(permissionedValidatorManager));

        // Try to accept as non-owner
        vm.startPrank(_randomCaller);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, _randomCaller));
        permissionedValidatorManager.acceptRegistryOwnership();
        vm.stopPrank();
    }

    function test_AcceptRegistryOwnership_RevertsIfNotPendingOwner() public {
        // PVM is not the pending owner
        address pendingOwner = registry.pendingOwner();

        // Verify PVM is not pending owner
        assertNotEq(pendingOwner, address(permissionedValidatorManager));

        // Try to accept ownership when not pending owner
        vm.startPrank(owner);
        vm.expectRevert(); // Will revert from Ownable2StepUpgradeable
        permissionedValidatorManager.acceptRegistryOwnership();
        vm.stopPrank();
    }

    function test_AcceptRegistryOwnership_WorksInMigrationScenario() public {
        // Simulate the migration scenario from the script
        // Step 1: Current registry owner transfers to new PVM
        address currentRegistryOwner = registry.owner();

        vm.prank(currentRegistryOwner);
        registry.transferOwnership(address(permissionedValidatorManager));

        // Verify pending state
        assertEq(registry.pendingOwner(), address(permissionedValidatorManager));
        assertEq(registry.owner(), currentRegistryOwner);

        // Step 2: PVM owner accepts ownership via PVM contract
        vm.startPrank(owner); // PVM owner

        // Expect RegistryOwnerTransferCompleted event
        vm.expectEmit();
        emit RegistryOwnerTransferCompleted();

        permissionedValidatorManager.acceptRegistryOwnership();
        vm.stopPrank();

        // Verify final state
        assertEq(registry.owner(), address(permissionedValidatorManager));
        assertEq(registry.pendingOwner(), address(0));

        // Add validator registerer first
        vm.prank(owner);
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);

        // Verify PVM can now call registry functions
        bytes memory newPublicKey = generateEd25519PublicKey(100);

        vm.prank(validatorRegisterer);
        uint256 registrationId = permissionedValidatorManager.registerValidator(newPublicKey);

        // Verify registration succeeded
        Validator memory validator = registry.getValidator(registrationId);
        assertEq(validator.publicKey, newPublicKey);
        assertEq(validator.votingPower, defaultVotingPower);
    }

    // ============ Helper Functions ============

    function _setPvmOwner(PermissionedValidatorManager pvm, address newOwner) internal {
        // Ownable2StepUpgradeable uses ERC-7201 slot for "openzeppelin.storage.Ownable"
        bytes32 ownableSlot = 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300;
        vm.store(address(pvm), ownableSlot, bytes32(uint256(uint160(newOwner))));
    }

    function _setPauser(address newPauser) internal {
        vm.prank(owner);
        permissionedValidatorManager.updatePauser(newPauser);
    }

    // ============ Pausable Tests ============

    function test_Pause_RevertsForNonPauser() public {
        _setPauser(pauser);

        vm.prank(notController);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        permissionedValidatorManager.pause();
    }

    function test_RegisterValidator_RevertsWhenPaused() public {
        vm.startPrank(owner);
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        vm.stopPrank();

        _setPauser(pauser);
        vm.prank(pauser);
        vm.expectEmit();
        emit Pause();
        permissionedValidatorManager.pause();
        assertTrue(permissionedValidatorManager.paused());

        vm.prank(validatorRegisterer);
        vm.expectRevert(Pausable.ContractPaused.selector);
        permissionedValidatorManager.registerValidator(validator1PublicKey);

        // Unpause and ensure register succeeds again
        vm.prank(pauser);
        vm.expectEmit();
        emit Unpause();
        permissionedValidatorManager.unpause();
        assertFalse(permissionedValidatorManager.paused());

        vm.prank(validatorRegisterer);
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(1, 0, validator1PublicKey);
        uint256 registrationId = permissionedValidatorManager.registerValidator(validator1PublicKey);
        assertEq(registrationId, 1);
    }

    function test_ControllerActions_RevertWhenPaused() public {
        vm.startPrank(owner);
        permissionedValidatorManager.addValidatorRegisterer(validatorRegisterer);
        permissionedValidatorManager.configureController(controller1, 1, controllerVotingPowerLimit);
        vm.stopPrank();

        vm.prank(validatorRegisterer);
        permissionedValidatorManager.registerValidator(validator1PublicKey);

        _setPauser(pauser);
        vm.prank(pauser);
        permissionedValidatorManager.pause();

        vm.prank(controller1);
        vm.expectRevert(Pausable.ContractPaused.selector);
        permissionedValidatorManager.activateValidator();

        vm.prank(controller1);
        vm.expectRevert(Pausable.ContractPaused.selector);
        permissionedValidatorManager.updateValidatorVotingPower(200);

        vm.prank(controller1);
        vm.expectRevert(Pausable.ContractPaused.selector);
        permissionedValidatorManager.removeValidator();
    }
}
