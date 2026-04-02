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

import {console} from "forge-std/Test.sol";
import {ValidatorRegistry} from "../../src/validator-manager/ValidatorRegistry.sol";
import {Validator, ValidatorStatus} from "../../src/validator-manager/interfaces/IValidatorRegistry.sol";
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";
import {TestUtils} from "./TestUtils.sol";

contract ValidatorRegistryTest is TestUtils {
    ValidatorRegistry public validatorRegistry;
    ValidatorRegistry public implementation;
    AdminUpgradeableProxy public proxy;

    address public owner;
    address public proxyOwner;

    // Test constants
    bytes validator1PublicKey;
    uint64 votingPower1 = 150;
    bytes validator2PublicKey;
    uint64 votingPower2 = 250;
    bytes validator3PublicKey;
    uint64 votingPower3 = 350;
    bytes validator4PublicKey;
    uint64 votingPower4 = 450;

    // ============ Constants ============
    uint256 private constant GENESIS_VALIDATOR_COUNT = 3;
    uint256 private constant EXPECTED_NEXT_REG_ID_AFTER_GENESIS = 4;

    // Storage layout constants
    uint256 private constant NEXT_REG_ID_SLOT_OFFSET = 4;
    uint256 private constant REGISTERED_VALIDATORS_SLOT_OFFSET = 3;
    uint256 private constant ACTIVE_VALIDATORS_SLOT_OFFSET = 1;

    // ============ Setup ============
    function setUp() public {
        owner = makeAddr("owner");
        proxyOwner = makeAddr("proxyOwner");

        // Initialize validator public keys
        validator1PublicKey = generateEd25519PublicKey(10);
        validator2PublicKey = generateEd25519PublicKey(20);
        validator3PublicKey = generateEd25519PublicKey(30);
        validator4PublicKey = generateEd25519PublicKey(40);
    }

    // ============ Tests ============

    // ============ Storage Layout Tests ============
    /**
     * @notice Test that the storage location constant follows ERC-7201 pattern and matches expected value
     */
    function test_StorageLocation_ERC7201Pattern_ShouldMatchExpectedValue() public {
        // Deploy ValidatorRegistry with initialValidators
        Validator[] memory initialValidators = initializeValidatorSet();
        ValidatorRegistry registry = deployValidatorRegistry(owner, initialValidators);

        // Calculate expected storage location using ERC-7201 formula
        // keccak256(abi.encode(uint256(keccak256("arc.storage.ValidatorRegistry")) - 1)) & ~bytes32(uint256(0xff))
        bytes32 namespace = keccak256("arc.storage.ValidatorRegistry");
        uint256 namespaceUint = uint256(namespace);
        bytes32 expectedLocation = keccak256(abi.encode(namespaceUint - 1)) & ~bytes32(uint256(0xff));

        // Verify that the contract constant matches the expected hardcoded value
        assertEq(
            registry.VALIDATOR_REGISTRY_STORAGE_LOCATION(),
            expectedLocation,
            "Storage location constant does not match expected hardcoded value"
        );
    }

    /**
     * @notice Test proxy deployment and admin functionality
     */
    function test_ProxyDeployment_ShouldSetupCorrectly() public {
        // Deploy ValidatorRegistry via proxy
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        // Verify proxy admin is set correctly
        assertEq(proxy.admin(), proxyOwner, "Proxy admin should be set correctly");

        // Verify proxy implementation is set correctly
        assertEq(proxy.implementation(), address(implementation), "Proxy implementation should be set correctly");

        // Verify the registry through proxy works correctly
        assertEq(registry.owner(), owner, "Registry owner should be set through proxy");

        // Verify we can interact with the registry through the proxy
        Validator[] memory activeValidators = registry.getActiveValidatorSet();
        assertEq(activeValidators.length, GENESIS_VALIDATOR_COUNT, "Should have genesis validators through proxy");
    }

    /**
     * @notice Test positive active voting power count initialization with mixed voting powers
     */
    function test_GetActiveValidatorsWithPositiveVotingPowerCount_WithMixedValidators_ShouldSetCorrectCount() public {
        Validator[] memory initialValidators = new Validator[](3);
        initialValidators[0] =
            Validator({status: ValidatorStatus.Active, publicKey: validator1PublicKey, votingPower: 100});
        initialValidators[1] =
            Validator({status: ValidatorStatus.Active, publicKey: validator2PublicKey, votingPower: 0});
        initialValidators[2] =
            Validator({status: ValidatorStatus.Active, publicKey: validator3PublicKey, votingPower: 300});

        ValidatorRegistry registry = deployValidatorRegistry(owner, initialValidators);
        uint256 positiveCount = registry.getActiveValidatorsWithPositiveVotingPowerCount();
        assertEq(positiveCount, 2, "Positive active voting power count should match initialized validators");
    }

    /**
     * @notice Test the actual implementation contract before genesis initialization
     */
    function test_ImplementationContractState_ShouldBeUninitialized() public {
        // Test the actual implementation contract before genesis initialization
        ValidatorRegistry impl = new ValidatorRegistry();

        // Implementation should be uninitialized (upgradeable pattern)
        assertEq(impl.owner(), address(0), "Implementation owner should be zero address (not initialized)");
        assertEq(
            impl.getNextRegistrationId(), 0, "Implementation should have default nextRegistrationId (0, uninitialized)"
        );

        // Active validator set should be empty
        Validator[] memory activeValidators = impl.getActiveValidatorSet();
        assertEq(activeValidators.length, 0, "Implementation should have no active validators");

        // Test getting non-existent validator
        Validator memory nonExistentValidator = impl.getValidator(1);
        assertEq(
            uint8(nonExistentValidator.status),
            uint8(ValidatorStatus.Unknown),
            "Implementation should return Unknown for non-existent validators"
        );
        assertEq(
            nonExistentValidator.votingPower,
            0,
            "Implementation should return zero voting power for non-existent validators"
        );
        assertEq(
            nonExistentValidator.publicKey.length,
            0,
            "Implementation should return empty public key for non-existent validators"
        );
    }

    /**
     * @notice Test function to demonstrate logGenesisStorageExample utility
     */
    function test_GenesisStorageLogging_WithSampleValidators_ShouldLogCorrectFormat() public view {
        // Create sample initial validators for logging
        Validator[] memory sampleValidators = new Validator[](2);

        sampleValidators[0] =
            Validator({status: ValidatorStatus.Active, publicKey: validator1PublicKey, votingPower: 100});

        sampleValidators[1] =
            Validator({status: ValidatorStatus.Active, publicKey: validator2PublicKey, votingPower: 200});

        // Call the logging utility function
        logGenesisStorageExample(owner, sampleValidators);
    }

    // ============ Genesis Initialization Tests ============
    /**
     * @notice Test function to demonstrate deployValidatorRegistry utility
     */
    function test_GenesisDeployment_WithInitialValidators_ShouldInitializeCorrectly() public {
        // Deploy ValidatorRegistry with initialValidators
        Validator[] memory initialValidators = initializeValidatorSet();
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        // Verify owner was set correctly
        assertEq(registry.owner(), owner, "Owner should be set correctly");

        // Verify validators were initialized correctly
        for (uint256 i = 0; i < initialValidators.length; i++) {
            uint256 registrationId = i + 1;
            Validator memory storedValidator = registry.getValidator(registrationId);

            assertEq(uint8(storedValidator.status), uint8(ValidatorStatus.Active), "Validator should be Active");
            assertEq(storedValidator.votingPower, initialValidators[i].votingPower, "Voting power should match");
            assertEq(storedValidator.publicKey, initialValidators[i].publicKey, "Public key should match");
        }

        // Verify active validator set
        Validator[] memory activeValidators = registry.getActiveValidatorSet();
        assertEq(activeValidators.length, GENESIS_VALIDATOR_COUNT, "Should have genesis validators");

        // Verify next registration ID
        assertEq(
            registry.getNextRegistrationId(),
            EXPECTED_NEXT_REG_ID_AFTER_GENESIS,
            "Next registration ID should be correct"
        );
    }

    // ============ View Function Tests ============
    /**
     * @notice Test getValidator with non-existent ID returns default values
     */
    function test_GetValidator_WithNonExistentID_ShouldReturnEmptyValidator() public {
        ValidatorRegistry registry = deployCleanValidatorRegistry();

        // Test getValidator with non-existent ID (edge case)
        Validator memory nonExistentValidator = registry.getValidator(999);

        assertEq(
            uint8(nonExistentValidator.status),
            uint8(ValidatorStatus.Unknown),
            "Non-existent validator should have Unknown status"
        );
        assertEq(nonExistentValidator.votingPower, 0, "Non-existent validator voting power should be zero");
        assertEq(nonExistentValidator.publicKey.length, 0, "Non-existent validator public key should be empty");
    }

    /**
     * @notice Test getActiveValidatorSet when no validators are active
     */
    function test_GetActiveValidatorSet_WhenEmpty_ShouldReturnEmptyArray() public {
        ValidatorRegistry registry = deployCleanValidatorRegistry();

        // Get active validator set when registry is empty
        Validator[] memory activeValidators = registry.getActiveValidatorSet();

        assertEq(activeValidators.length, 0, "Active validator set should be empty");
    }

    /**
     * @notice Test that view functions work when there are no active validators and activation remains possible
     */
    function test_ViewFunctions_WhenNoActiveValidators_ShouldWork_AndActivationShouldStillBePossible() public {
        ValidatorRegistry registry = deployCleanValidatorRegistry();

        // View functions should still work in an empty/zero-active setup.
        Validator[] memory activeValidatorsBefore = registry.getActiveValidatorSet();
        assertEq(activeValidatorsBefore.length, 0, "Active validator set should start empty");
        assertEq(
            registry.getActiveValidatorsWithPositiveVotingPowerCount(),
            0,
            "Positive active voting power count should be zero"
        );
        assertEq(registry.getNextRegistrationId(), 1, "Next registration ID should start at 1");

        // Register and activate a validator after starting from zero active validators.
        vm.startPrank(owner);
        uint256 registrationId = registry.registerValidator(validator4PublicKey, votingPower4);
        registry.activateValidator(registrationId);
        vm.stopPrank();

        Validator[] memory activeValidatorsAfter = registry.getActiveValidatorSet();
        assertEq(activeValidatorsAfter.length, 1, "Activation should succeed from zero active validators");
        assertEq(
            registry.getActiveValidatorsWithPositiveVotingPowerCount(),
            1,
            "Positive active voting power count should update after activation"
        );

        Validator memory activatedValidator = registry.getValidator(registrationId);
        assertEq(uint8(activatedValidator.status), uint8(ValidatorStatus.Active), "Validator should be active");
    }

    /**
     * @notice Test getActiveValidatorSet cannot be emptied by removing all validators
     */
    function test_GetActiveValidatorSet_WhenAttemptingToRemoveAll_ShouldKeepOneValidator() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Verify we start with genesis validators
        Validator[] memory initialActiveValidators = registry.getActiveValidatorSet();
        assertEq(initialActiveValidators.length, GENESIS_VALIDATOR_COUNT, "Should start with genesis validators");

        // Remove two validators successfully
        for (uint256 i = 1; i < GENESIS_VALIDATOR_COUNT; i++) {
            registry.removeValidator(i);
        }

        // Removing the final active validator should fail
        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        registry.removeValidator(GENESIS_VALIDATOR_COUNT);

        // Verify active validator set retains one validator
        Validator[] memory finalActiveValidators = registry.getActiveValidatorSet();
        assertEq(finalActiveValidators.length, 1, "Active validator set should retain one validator");

        vm.stopPrank();
    }

    // ============ Validator Registration Tests ============
    /**
     * @notice Test successful validator registration
     */
    function test_RegisterValidator_WithValidData_ShouldSucceedAndEmitEvent() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.prank(owner);

        // Expect ValidatorRegistered event
        vm.expectEmit(true, false, false, true);
        emit ValidatorRegistered(EXPECTED_NEXT_REG_ID_AFTER_GENESIS, votingPower4, validator4PublicKey);

        uint256 registrationId = registry.registerValidator(validator4PublicKey, votingPower4);

        assertEq(registrationId, EXPECTED_NEXT_REG_ID_AFTER_GENESIS, "Registration ID should be correct");

        // Verify validator was stored correctly
        Validator memory storedValidator = registry.getValidator(registrationId);
        assertEq(
            uint8(storedValidator.status), uint8(ValidatorStatus.Registered), "Validator should be in Registered status"
        );
        assertEq(storedValidator.publicKey, validator4PublicKey, "Public key should match");
        assertEq(storedValidator.votingPower, votingPower4, "Voting power should match");
    }

    /**
     * @notice Test registering validator that is already registered
     */
    function test_RegisterValidator_WithAlreadyRegisteredValidator_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Registration should fail because validator1 public key is already in genesis
        vm.expectRevert(abi.encodeWithSelector(ValidatorRegistry.ValidatorAlreadyRegistered.selector, keccak256(validator1PublicKey)));
        registry.registerValidator(validator1PublicKey, 200);

        vm.stopPrank();
    }

    function test_RegisterValidator_WithInvalidKeyFormat_ShouldRevert(bytes calldata _invalidKey, uint64 _votingPower)
        public
    {
        // See: ValidatorRegistry.ED25519_PUBLIC_KEY_LENGTH;
        vm.assume(_invalidKey.length != 32);
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        vm.expectRevert(ValidatorRegistry.InvalidPublicKeyFormat.selector);
        _validatorRegistry.registerValidator(_invalidKey, _votingPower);

        vm.stopPrank();
    }

    // ============ Access Control Tests ============
    /**
     * @notice Test that non-owner cannot register validators
     */
    function test_RegisterValidator_WithNonOwner_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();
        address nonOwner = address(0x456);

        vm.prank(nonOwner);
        vm.expectRevert(abi.encodeWithSignature("OwnableUnauthorizedAccount(address)", nonOwner));
        registry.registerValidator(validator4PublicKey, votingPower4);
    }

    /**
     * @notice Test that non-owner cannot activate validators
     */
    function test_ActivateValidator_WithNonOwner_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();
        address nonOwner = address(0x456);

        // First register a validator as owner
        vm.prank(owner);
        uint256 registrationId = registry.registerValidator(validator4PublicKey, votingPower4);

        // Try to activate as non-owner
        vm.prank(nonOwner);
        vm.expectRevert(abi.encodeWithSignature("OwnableUnauthorizedAccount(address)", nonOwner));
        registry.activateValidator(registrationId);
    }

    /**
     * @notice Test that non-owner cannot remove validators
     */
    function test_RemoveValidator_WithNonOwner_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();
        address nonOwner = address(0x456);

        uint256 registrationIdToRemove = 1; // Genesis validator

        vm.prank(nonOwner);
        vm.expectRevert(abi.encodeWithSignature("OwnableUnauthorizedAccount(address)", nonOwner));
        registry.removeValidator(registrationIdToRemove);
    }

    /**
     * @notice Test that non-owner cannot update validator voting power
     */
    function test_UpdateValidatorVotingPower_WithNonOwner_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();
        address nonOwner = address(0x456);

        uint256 registrationId = 1; // Genesis validator
        uint64 newVotingPower = 200;

        vm.prank(nonOwner);
        vm.expectRevert(abi.encodeWithSignature("OwnableUnauthorizedAccount(address)", nonOwner));
        registry.updateValidatorVotingPower(registrationId, newVotingPower);
    }

    // ============ Validator Activation Tests ============
    /**
     * @notice Test successful validator activation
     */
    function test_ActivateValidator_WithRegisteredValidator_ShouldSucceedAndEmitEvent() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // First register a validator
        uint256 registrationId = registry.registerValidator(validator4PublicKey, votingPower4);
        assertEq(registrationId, EXPECTED_NEXT_REG_ID_AFTER_GENESIS, "Registration ID should be correct");

        // Expect ValidatorActivated event
        vm.expectEmit(true, true, false, true);
        emit ValidatorActivated(registrationId, votingPower4);

        registry.activateValidator(registrationId);

        // Verify validator status changed to Active
        Validator memory activatedValidator = registry.getValidator(registrationId);
        assertEq(uint8(activatedValidator.status), uint8(ValidatorStatus.Active), "Validator should be Active");

        // Verify validator appears in active set
        Validator[] memory activeValidators = registry.getActiveValidatorSet();
        assertEq(activeValidators.length, GENESIS_VALIDATOR_COUNT + 1, "Should have genesis + 1 active validators");

        vm.stopPrank();
    }

    /**
     * @notice Test activating validator with invalid registration ID
     */
    function test_ActivateValidator_WithInvalidRegistrationId_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        vm.prank(owner);

        // Try to activate non-existent validator
        vm.expectRevert(abi.encodeWithSelector(ValidatorRegistry.InvalidRegistrationId.selector, 999));
        _validatorRegistry.activateValidator(999);
    }

    /**
     * @notice Test activating validator that is already active
     */
    function test_ActivateValidator_WithAlreadyActiveValidator_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Register a new validator
        uint256 registrationId = _validatorRegistry.registerValidator(validator4PublicKey, votingPower4);

        // Activate the validator successfully
        _validatorRegistry.activateValidator(registrationId);

        // Try to activate the same validator again (should fail)
        vm.expectRevert(abi.encodeWithSelector(ValidatorRegistry.InvalidRegistrationId.selector, registrationId));
        _validatorRegistry.activateValidator(registrationId);

        vm.stopPrank();
    }

    // ============ Validator Removal Tests ============
    /**
     * @notice Test successful validator removal
     */
    function test_RemoveValidator_WithActiveValidator_ShouldSucceedAndEmitEvent() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Verify validator is active
        Validator[] memory activeValidators = registry.getActiveValidatorSet();
        assertEq(activeValidators.length, GENESIS_VALIDATOR_COUNT, "Should have genesis validators initially");

        uint256 registrationIdToRemove = 1;
        // Expect ValidatorRemoved event
        vm.expectEmit(true, true, false, true);
        emit ValidatorRemoved(registrationIdToRemove, votingPower1);

        registry.removeValidator(registrationIdToRemove);

        // Verify validator was removed
        Validator memory removedValidator = registry.getValidator(registrationIdToRemove);
        assertEq(
            uint8(removedValidator.status), uint8(ValidatorStatus.Unknown), "Validator should be Unknown after removal"
        );

        // Verify active validator set now has 2 validators
        Validator[] memory activeValidatorsAfter = registry.getActiveValidatorSet();
        assertEq(
            activeValidatorsAfter.length, GENESIS_VALIDATOR_COUNT - 1, "Should have one less validator after removal"
        );

        vm.stopPrank();
    }

    /**
     * @notice Test removing validator with invalid registration ID
     */
    function test_RemoveValidator_WithInvalidRegistrationId_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        vm.prank(owner);

        // Try to remove non-existent validator
        vm.expectRevert(abi.encodeWithSelector(ValidatorRegistry.InvalidRegistrationId.selector, 999));
        _validatorRegistry.removeValidator(999);
    }

    // ============ Voting Power Update Tests ============
    /**
     * @notice Test successful voting power update with event emission and getValidator verification
     */
    function test_UpdateValidatorVotingPower_WithValidNewPower_ShouldSucceedAndEmitEvent() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        uint256 registrationId = 1;
        uint64 newVotingPower = 200;

        // Update voting power and expect event
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, votingPower1, newVotingPower);

        vm.prank(owner);
        registry.updateValidatorVotingPower(registrationId, newVotingPower);

        // Verify the voting power was updated using getValidator
        Validator memory updatedValidator = registry.getValidator(registrationId);
        assertEq(updatedValidator.votingPower, newVotingPower, "Voting power should be updated to 200");

        // Verify other fields remain unchanged
        assertEq(uint8(updatedValidator.status), uint8(ValidatorStatus.Active), "Status should remain unchanged");
        assertEq(updatedValidator.publicKey, validator1PublicKey, "Public key should remain unchanged");
    }

    /**
     * @notice Test voting power update to zero (effectively deactivates the validator)
     */
    function test_UpdateValidatorVotingPower_WithZeroVotingPower_ShouldSucceedAndDeactivate() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        uint256 registrationId = 1;

        // Update voting power to zero (should succeed and deactivate validator)
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, votingPower1, 0);

        vm.prank(owner);
        registry.updateValidatorVotingPower(registrationId, 0);

        // Verify voting power was updated to zero (validator is deactivated)
        Validator memory validator = registry.getValidator(registrationId);
        assertEq(validator.votingPower, 0, "Voting power should be updated to zero (deactivated)");
        assertEq(uint8(validator.status), uint8(ValidatorStatus.Active), "Status should remain active");
    }

    /**
     * @notice Test updating voting power from zero to positive for an active validator
     */
    function test_UpdateValidatorVotingPower_ZeroToPositiveForActiveValidator_ShouldSucceed() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Register a validator with zero voting power and activate it
        uint256 registrationId = registry.registerValidator(validator4PublicKey, 0);
        registry.activateValidator(registrationId);

        uint64 newVotingPower = 125;
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, 0, newVotingPower);
        registry.updateValidatorVotingPower(registrationId, newVotingPower);

        Validator memory updatedValidator = registry.getValidator(registrationId);
        assertEq(updatedValidator.votingPower, newVotingPower, "Voting power should update from zero to positive");
        assertEq(uint8(updatedValidator.status), uint8(ValidatorStatus.Active), "Validator should remain active");

        vm.stopPrank();
    }

    /**
     * @notice Test updating voting power for non-existent validator
     */
    function test_UpdateValidatorVotingPower_WithNonExistentValidator_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        uint256 nonExistentId = 999;

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(ValidatorRegistry.InvalidRegistrationId.selector, nonExistentId));
        _validatorRegistry.updateValidatorVotingPower(nonExistentId, 100);
    }

    /**
     * @notice Test updating voting power for removed validator
     */
    function test_UpdateValidatorVotingPower_WithRemovedValidator_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        // Register and then remove a validator
        vm.startPrank(owner);
        uint256 registrationIdToRemove = 1;
        _validatorRegistry.removeValidator(registrationIdToRemove);

        // Try to update voting power for removed validator (should fail)
        vm.expectRevert(
            abi.encodeWithSelector(ValidatorRegistry.InvalidRegistrationId.selector, registrationIdToRemove)
        );
        _validatorRegistry.updateValidatorVotingPower(registrationIdToRemove, 200);
        vm.stopPrank();
    }

    /**
     * @notice Test updating voting power to the same value (should revert)
     */
    function test_UpdateValidatorVotingPower_WithSameValue_ShouldRevert() public {
        ValidatorRegistry _validatorRegistry = deployGenesisValidatorRegistry();

        uint256 registrationId = 1;

        // Try to update voting power to the same value (should fail)
        vm.prank(owner);
        vm.expectRevert(ValidatorRegistry.InvalidVotingPowerUpdate.selector);
        _validatorRegistry.updateValidatorVotingPower(registrationId, votingPower1);

        // Verify voting power remains unchanged
        Validator memory validator = _validatorRegistry.getValidator(registrationId);
        assertEq(validator.votingPower, votingPower1, "Voting power should remain unchanged after failed update");
    }

    /**
     * @notice Test multiple voting power updates
     */
    function test_UpdateValidatorVotingPower_WithMultipleUpdates_ShouldSucceedSequentially() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        uint256 registrationId = 1;

        // Perform multiple updates
        vm.startPrank(owner);

        // First update: 150 -> 200
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, votingPower1, 200);
        registry.updateValidatorVotingPower(registrationId, 200);

        // Second update: 200 -> 150
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, 200, 150);
        registry.updateValidatorVotingPower(registrationId, 150);

        // Third update: 150 -> 300
        vm.expectEmit(true, true, false, true);
        emit ValidatorVotingPowerUpdated(registrationId, 150, 300);
        registry.updateValidatorVotingPower(registrationId, 300);

        vm.stopPrank();

        // Verify final voting power
        Validator memory finalValidator = registry.getValidator(registrationId);
        assertEq(finalValidator.votingPower, 300, "Final voting power should be 300");
    }

    /**
     * @notice Test setting the last active validator with non-zero voting power to zero should revert
     */
    function test_UpdateValidatorVotingPower_SetLastPositiveActiveVotingPowerToZero_ShouldRevert() public {
        ValidatorRegistry registry = deployGenesisValidatorRegistry();

        vm.startPrank(owner);

        // Reduce two active validators to zero voting power
        registry.updateValidatorVotingPower(2, 0);
        registry.updateValidatorVotingPower(3, 0);

        // Zeroing the last active validator with non-zero voting power should fail
        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        registry.updateValidatorVotingPower(1, 0);

        vm.stopPrank();
    }

    // ============ Genesis Helper Functions ============
    // Helper function to deploy ValidatorRegistry with custom parameters (simulating genesis initialization)
    function deployCleanValidatorRegistry() internal returns (ValidatorRegistry) {
        // Deploy implementation contract
        implementation = new ValidatorRegistry();

        // Deploy proxy without initialization (empty data)
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyOwner,
            "" // No initialization data - will be set through genesis-style storage manipulation
        );

        // Get the proxy as ValidatorRegistry interface
        ValidatorRegistry deployedRegistry = ValidatorRegistry(address(proxy));

        // Simulate genesis file initialization by directly setting storage using calculated indices
        _simulateGenesisStorageInitialization(deployedRegistry, owner, new Validator[](0));

        assertEq(
            deployedRegistry.getActiveValidatorsWithPositiveVotingPowerCount(),
            0,
            "Positive active voting power count should be zero for empty validator set"
        );

        return deployedRegistry;
    }

    function deployGenesisValidatorRegistry() internal returns (ValidatorRegistry) {
        return deployValidatorRegistry(owner, initializeValidatorSet());
    }

    function deployValidatorRegistry(address _owner, Validator[] memory _initialValidators)
        internal
        returns (ValidatorRegistry)
    {
        // Deploy implementation contract
        implementation = new ValidatorRegistry();

        // Deploy proxy without initialization (empty data)
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyOwner,
            "" // No initialization data - will be set through genesis-style storage manipulation
        );

        // Get the proxy as ValidatorRegistry interface
        ValidatorRegistry deployedRegistry = ValidatorRegistry(address(proxy));

        // Simulate genesis file initialization by directly setting storage using calculated indices
        _simulateGenesisStorageInitialization(deployedRegistry, _owner, _initialValidators);

        uint256 expectedPositiveVotingPowerCount = 0;
        for (uint256 i = 0; i < _initialValidators.length; i++) {
            if (_initialValidators[i].votingPower > 0) {
                ++expectedPositiveVotingPowerCount;
            }
        }
        assertEq(
            deployedRegistry.getActiveValidatorsWithPositiveVotingPowerCount(),
            expectedPositiveVotingPowerCount,
            "Positive active voting power count should match initialized validators"
        );

        return deployedRegistry;
    }

    function initializeValidatorSet() internal view returns (Validator[] memory) {
        // Create initial validators for genesis (used by specific tests)
        Validator[] memory genesisValidators = new Validator[](3);
        genesisValidators[0] = Validator({
            status: ValidatorStatus.Active, // Will be overridden in genesis
            publicKey: validator1PublicKey,
            votingPower: votingPower1
        });
        genesisValidators[1] =
            Validator({status: ValidatorStatus.Active, publicKey: validator2PublicKey, votingPower: votingPower2});
        genesisValidators[2] =
            Validator({status: ValidatorStatus.Active, publicKey: validator3PublicKey, votingPower: votingPower3});
        return genesisValidators;
    }

    // Helper function to simulate genesis file initialization using calculated storage indices
    function _simulateGenesisStorageInitialization(
        ValidatorRegistry registry,
        address _owner,
        Validator[] memory _initialValidators
    ) internal {
        // This simulates how genesis file would set storage slots directly

        // === Set Ownable storage (ERC-7201) ===
        // ValidatorRegistry now uses Ownable2StepUpgradeable, so owner is stored at ERC-7201 slot
        bytes32 ownableSlot = 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300;
        vm.store(address(registry), ownableSlot, bytes32(uint256(uint160(_owner))));

        // === Set ERC-7201 ValidatorRegistry storage ===
        // Base slot: 0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200
        bytes32 baseSlot = registry.VALIDATOR_REGISTRY_STORAGE_LOCATION();

        // Set nextRegistrationID and initialization flag
        _setRegistryMetadata(registry, baseSlot, _initialValidators.length);

        // Set initial validators data
        for (uint256 i = 0; i < _initialValidators.length; i++) {
            _setValidatorData(registry, baseSlot, i + 1, _initialValidators[i]);
        }
    }

    // Helper function to set registry metadata
    function _setRegistryMetadata(ValidatorRegistry registry, bytes32 baseSlot, uint256 validatorCount) private {
        // Set nextRegistrationID (base slot + NEXT_REG_ID_SLOT_OFFSET)
        bytes32 nextRegSlot = bytes32(uint256(baseSlot) + NEXT_REG_ID_SLOT_OFFSET);
        bytes32 valueToStore = bytes32(validatorCount + 1);
        vm.store(address(registry), nextRegSlot, valueToStore);

        // Set up EnumerableSet for _activeValidatorRegistrations (base slot + ACTIVE_VALIDATORS_SLOT_OFFSET)
        bytes32 activeSetSlot = bytes32(uint256(baseSlot) + ACTIVE_VALIDATORS_SLOT_OFFSET);

        // EnumerableSet internal structure:
        // - _values array starts at keccak256(activeSetSlot)
        // - _indexes mapping starts at activeSetSlot + 1

        // Set the length of the _values array
        vm.store(address(registry), activeSetSlot, bytes32(validatorCount));

        // Set the values in the _values array and corresponding _indexes mapping
        bytes32 valuesSlot = keccak256(abi.encode(activeSetSlot));
        bytes32 indexesSlot = bytes32(uint256(activeSetSlot) + 1);

        for (uint256 i = 0; i < validatorCount; i++) {
            uint256 registrationId = i + 1;

            // Set value at index i in _values array
            vm.store(address(registry), bytes32(uint256(valuesSlot) + i), bytes32(registrationId));

            // Set index mapping: _indexes[registrationId] = i + 1 (1-based indexing)
            bytes32 indexMappingSlot = keccak256(abi.encode(registrationId, indexesSlot));
            vm.store(address(registry), indexMappingSlot, bytes32(i + 1));
        }
    }

    // Helper function to set individual validator data
    function _setValidatorData(
        ValidatorRegistry registry,
        bytes32 baseSlot,
        uint256 registrationId,
        Validator memory validator
    ) private {
        // Validator mapping slot
        bytes32 validatorSlot = keccak256(abi.encode(registrationId, baseSlot));

        // Set validator struct fields according to Solidity struct packing:
        // Slot 0: status (uint8) only
        bytes32 packedSlot0 = bytes32(uint256(2)); // ValidatorStatus.Active = 2
        vm.store(address(registry), validatorSlot, packedSlot0);

        // Slot 1: publicKey (bytes) - Ed25519 keys are always 32 bytes
        // For 32-byte bytes, stored as: length in slot 1, data in keccak256(slot 1)
        vm.store(address(registry), bytes32(uint256(validatorSlot) + 1), bytes32(uint256(64) + 1)); // length * 2 + 1 = 32 * 2 + 1 = 65
        bytes32 dataSlot = keccak256(abi.encode(uint256(validatorSlot) + 1));
        vm.store(address(registry), dataSlot, bytes32(validator.publicKey));

        // Slot 2: votingPower (uint64)
        vm.store(address(registry), bytes32(uint256(validatorSlot) + 2), bytes32(uint256(validator.votingPower)));

        // Set registered public keys mapping (_registeredPublicKeys[keccak256(publicKey)] = true)
        // _registeredPublicKeys is at REGISTERED_VALIDATORS_SLOT_OFFSET
        bytes32 registeredPublicKeysSlot = bytes32(uint256(baseSlot) + REGISTERED_VALIDATORS_SLOT_OFFSET);
        bytes32 publicKeyHash = keccak256(validator.publicKey);
        bytes32 registeredSlot = keccak256(abi.encode(publicKeyHash, registeredPublicKeysSlot));

        vm.store(address(registry), registeredSlot, bytes32(uint256(1)));
    }

    // ============ Genesis Storage Logging Utility ============

    /**
     * @notice Utility function to log genesis storage allocation for ValidatorRegistry
     * @param _owner The owner address of the ValidatorRegistry
     * @param _initialValidators Array of initial validators for genesis
     */
    function logGenesisStorageExample(address _owner, Validator[] memory _initialValidators) public pure {
        // Log each storage slot individually to avoid stack too deep

        // Standard contract storage
        console.log("=== ValidatorRegistry Genesis Storage Allocation ===");
        console.log("// Owner (ERC-7201 slot for openzeppelin.storage.Ownable)");
        console.log(
            '"0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300": "0x%s",', _toHexStringAddress(_owner)
        );

        // ValidatorRegistryStorage struct layout (ERC-7201):
        // struct ValidatorRegistryStorage {
        //     mapping(uint256 => Validator) _validatorsByRegistrationId;  // slot 0 (uses keccak256 hashing)
        //     EnumerableSet.UintSet _activeValidatorRegistrations;        // slot 1-2 (2 slots: _values array + _positions mapping)
        //     mapping(bytes32 => bool) _registeredPublicKeys;             // slot 3 (uses keccak256 hashing)
        //     uint256 _nextRegistrationID;                                // slot 4 (EnumerableSet uses 2 slots)
        // }
        console.log("// ValidatorRegistry Storage Location (ERC-7201)");
        console.log(
            "// keccak256(abi.encode(uint256(keccak256(\"arc.storage.ValidatorRegistry\")) - 1)) & ~bytes32(uint256(0xff))"
        );
        console.log("// Base storage slot: 0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200");

        // nextRegistrationID (base slot + 4)
        uint256 nextRegId = _initialValidators.length + 1;
        console.log("// nextRegistrationID (base slot + 4)");
        console.log(
            '"0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d204": "0x%s",',
            _toHexStringBytes32(bytes32(nextRegId))
        );

        // Log initial validators data
        for (uint256 i = 0; i < _initialValidators.length; i++) {
            uint256 registrationId = i + 1;

            console.log("// Validator %d Data (registration ID %d)", i + 1, registrationId);

            // Validator mapping slot calculation: keccak256(registrationId . baseSlot)
            bytes32 validatorSlot = keccak256(
                abi.encode(registrationId, 0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200)
            );

            // Status (Active = 2)
            console.log(
                '"0x%s": "0x0000000000000000000000000000000000000000000000000000000000000002",',
                _toHexStringBytes32(validatorSlot)
            );

            // Public key (slot + 1) - first 32 bytes
            bytes32 pubKeySlot = bytes32(uint256(validatorSlot) + 1);
            if (_initialValidators[i].publicKey.length >= 32) {
                bytes32 pubKeyValue = bytes32(_initialValidators[i].publicKey);
                console.log('"0x%s": "0x%s",', _toHexStringBytes32(pubKeySlot), _toHexStringBytes32(pubKeyValue));
            }

            // Voting power (slot + 2)
            bytes32 votingPowerSlot = bytes32(uint256(validatorSlot) + 2);
            console.log(
                '"0x%s": "0x%s",',
                _toHexStringBytes32(votingPowerSlot),
                _toHexStringBytes32(bytes32(uint256(_initialValidators[i].votingPower)))
            );
        }

        console.log("=== End ValidatorRegistry Genesis Storage ===");
    }

    // ============ Hex String Helper Functions ============

    /**
     * @notice Convert an address to a hex string without 0x prefix
     */
    function _toHexStringAddress(address addr) internal pure returns (string memory) {
        return _toHexStringBytes32(bytes32(uint256(uint160(addr))));
    }

    /**
     * @notice Convert bytes32 to a hex string without 0x prefix
     */
    function _toHexStringBytes32(bytes32 value) internal pure returns (string memory) {
        bytes memory buffer = new bytes(64);
        bytes memory alphabet = "0123456789abcdef";

        for (uint256 i = 0; i < 32; i++) {
            buffer[i * 2] = alphabet[uint8(value[i] >> 4)];
            buffer[i * 2 + 1] = alphabet[uint8(value[i] & 0x0f)];
        }

        return string(buffer);
    }
}
