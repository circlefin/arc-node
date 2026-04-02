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

import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";
import {ValidatorRegistry} from "../../src/validator-manager/ValidatorRegistry.sol";
import {Validator, ValidatorStatus} from "../../src/validator-manager/interfaces/IValidatorRegistry.sol";
import {TestUtils} from "./TestUtils.sol";

/**
 * @title ValidatorRegistryProxyTest
 * @dev Test suite for ValidatorRegistry when deployed behind AdminUpgradeableProxy
 * @dev Tests ValidatorRegistry-specific functionality and business logic through proxy
 */
contract ValidatorRegistryProxyTest is TestUtils {
    // ============ Constants ============
    bytes32 private constant VALIDATOR_REGISTRY_STORAGE_LOCATION =
        0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200;

    // ============ State Variables ============

    AdminUpgradeableProxy public proxy;
    ValidatorRegistry public implementation;
    ValidatorRegistry public validatorRegistry;
    address public actualProxyAdmin; // The actual admin address read from proxy storage

    // Test role addresses
    address public proxyAdminAddress; // The address we set as proxy admin
    address public registryOwner; // Owner role for ValidatorRegistry
    address public implementationOwner; // Owner of the ValidatorRegistry implementation

    // Test validator data
    bytes public validator1PublicKey;
    bytes public validator2PublicKey;
    bytes public validator3PublicKey;
    uint64 public constant VOTING_POWER_1 = 100;
    uint64 public constant VOTING_POWER_2 = 200;
    uint64 public constant VOTING_POWER_3 = 300;

    // ============ Setup ============

    function setUp() public {
        // Create test addresses
        proxyAdminAddress = makeAddr("proxyAdminAddress");
        registryOwner = makeAddr("registryOwner");
        implementationOwner = makeAddr("implementationOwner");

        // Generate test validator public keys
        validator1PublicKey = generateEd25519PublicKey(1);
        validator2PublicKey = generateEd25519PublicKey(2);
        validator3PublicKey = generateEd25519PublicKey(3);

        // Deploy implementation contract
        implementation = new ValidatorRegistry();

        // Deploy proxy without initialization data (will be set via storage manipulation)
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyAdminAddress,
            "" // No initialization data - will be set through genesis-style storage manipulation
        );

        // Get proxy as ValidatorRegistry interface
        validatorRegistry = ValidatorRegistry(address(proxy));

        // Simulate genesis file initialization by directly setting storage
        _simulateGenesisStorageInitialization(registryOwner);

        // Get the actual proxy admin address from ERC1967 storage
        actualProxyAdmin = address(
            uint160(
                uint256(vm.load(address(proxy), 0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103))
            )
        );
    }

    // Helper function to simulate genesis file initialization using storage manipulation
    function _simulateGenesisStorageInitialization(address _owner) internal {
        // === Set Ownable owner in ERC-7201 storage ===
        // Calculate the correct ERC-7201 storage slot for Ownable owner
        // Pattern: keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff))
        bytes32 ownableStorageSlot =
            keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff));
        vm.store(address(validatorRegistry), ownableStorageSlot, bytes32(uint256(uint160(_owner))));

        // === Set ValidatorRegistry ERC-7201 storage ===
        // Initialize _nextRegistrationId to 1 (slot offset 4 in ValidatorRegistryStorage)
        vm.store(
            address(validatorRegistry), bytes32(uint256(VALIDATOR_REGISTRY_STORAGE_LOCATION) + 4), bytes32(uint256(1))
        );

        // Note: Other mappings (_validatorsByRegistrationId, _registeredPublicKeys) and
        // EnumerableSet (_activeValidatorRegistrations) are initialized empty by default
    }

    // ============ ValidatorRegistry Functionality Tests ============

    function test_ValidatorRegistryInitialization() public view {
        // Verify ValidatorRegistry is initialized correctly through proxy
        assertEq(validatorRegistry.getNextRegistrationId(), 1, "Next registration ID should be 1");

        // Verify active validator set is empty initially
        Validator[] memory activeValidators = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidators.length, 0, "Active validator set should be empty");
    }

    function test_GetActiveValidatorsWithPositiveVotingPowerCount_WithMixedActiveValidators_ShouldSetCorrectCount()
        public
    {
        // Register validators with mixed voting powers and activate them
        vm.startPrank(registryOwner);
        uint256 regId1 = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1); // positive
        uint256 regId2 = validatorRegistry.registerValidator(validator2PublicKey, 0); // zero
        uint256 regId3 = validatorRegistry.registerValidator(validator3PublicKey, VOTING_POWER_3); // positive
        validatorRegistry.activateValidator(regId1);
        validatorRegistry.activateValidator(regId2);
        validatorRegistry.activateValidator(regId3);
        vm.stopPrank();

        // Count should reflect active validators with positive voting power.
        assertEq(
            validatorRegistry.getActiveValidatorsWithPositiveVotingPowerCount(),
            2,
            "Positive active voting power count should match active validators"
        );
    }

    function test_RegisterValidatorViaProxy() public {
        // Test registering a validator through the proxy
        vm.prank(registryOwner);
        uint256 registrationId = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        assertEq(registrationId, 1, "First registration ID should be 1");
        assertEq(validatorRegistry.getNextRegistrationId(), 2, "Next registration ID should be 2");

        // Verify validator data
        Validator memory registeredValidator = validatorRegistry.getValidator(registrationId);
        assertEq(
            uint256(registeredValidator.status), uint256(ValidatorStatus.Registered), "Validator should be registered"
        );
        assertEq(registeredValidator.publicKey, validator1PublicKey, "Public key should match");
        assertEq(registeredValidator.votingPower, VOTING_POWER_1, "Voting power should match");
    }

    function test_ActivateValidatorViaProxy() public {
        // Register a validator first
        vm.prank(registryOwner);
        uint256 registrationId = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        // Activate the validator
        vm.prank(registryOwner);
        validatorRegistry.activateValidator(registrationId);

        // Verify validator is active
        Validator memory activeValidator = validatorRegistry.getValidator(registrationId);
        assertEq(uint256(activeValidator.status), uint256(ValidatorStatus.Active), "Validator should be active");

        // Verify active validator set contains the validator
        Validator[] memory activeValidators = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidators.length, 1, "Active validator set should contain 1 validator");
        assertEq(activeValidators[0].publicKey, validator1PublicKey, "Active validator public key should match");
    }

    function test_UpdateValidatorVotingPowerViaProxy() public {
        // Register and activate a validator
        vm.prank(registryOwner);
        uint256 registrationId = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        vm.prank(registryOwner);
        validatorRegistry.activateValidator(registrationId);

        // Update voting power
        uint64 newVotingPower = 500;
        vm.prank(registryOwner);
        validatorRegistry.updateValidatorVotingPower(registrationId, newVotingPower);

        // Verify voting power updated
        Validator memory updatedValidator = validatorRegistry.getValidator(registrationId);
        assertEq(updatedValidator.votingPower, newVotingPower, "Voting power should be updated");
    }

    function test_RemoveValidatorViaProxy_WhenRemovingLastActiveValidator_ShouldRevert() public {
        // Register and activate a validator
        vm.prank(registryOwner);
        uint256 registrationId = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        vm.prank(registryOwner);
        validatorRegistry.activateValidator(registrationId);

        vm.prank(registryOwner);
        uint256 registrationId2 = validatorRegistry.registerValidator(validator2PublicKey, VOTING_POWER_2);

        vm.prank(registryOwner);
        validatorRegistry.activateValidator(registrationId2);

        // Verify validators are active initially
        Validator[] memory activeValidatorsBefore = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidatorsBefore.length, 2, "Should have 2 active validators before removal");

        // Removing one active validator should succeed
        vm.prank(registryOwner);
        validatorRegistry.removeValidator(registrationId);

        Validator[] memory activeValidatorsAfterFirstRemoval = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidatorsAfterFirstRemoval.length, 1, "Active validator set should contain 1 validator");

        // Removing the last active validator should fail
        vm.prank(registryOwner);
        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        validatorRegistry.removeValidator(registrationId2);

        // Verify the remaining validator is unchanged after failed removal
        Validator memory remainingValidator = validatorRegistry.getValidator(registrationId2);
        assertEq(
            uint256(remainingValidator.status),
            uint256(ValidatorStatus.Active),
            "Remaining validator should stay active after failed removal"
        );

        // Verify active validator set is unchanged
        Validator[] memory activeValidatorsAfterFailedRemoval = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidatorsAfterFailedRemoval.length, 1, "Active validator set should still contain 1 validator");
    }

    function test_AccessControlViaProxy() public {
        // Test that access control works through the proxy
        address unauthorizedUser = makeAddr("unauthorizedUser");

        // Non-owner should not be able to register validators
        vm.prank(unauthorizedUser);
        vm.expectRevert(); // Should revert with access control error
        validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        // Owner should be able to register validators
        vm.prank(registryOwner);
        uint256 registrationId = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1); // Should succeed
        assertEq(registrationId, 1, "Registration should succeed for owner");
    }

    // ============ ValidatorRegistry Upgrade Tests ============

    function test_ValidatorRegistryUpgradeWithInitialization() public {
        // Register some validators first
        vm.startPrank(registryOwner);
        validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);
        validatorRegistry.registerValidator(validator2PublicKey, VOTING_POWER_2);
        vm.stopPrank();

        // Deploy a new ValidatorRegistry implementation
        ValidatorRegistry newImplementation = new ValidatorRegistry();

        // Transfer ownership to proxy admin so they can make the initialization call
        vm.prank(registryOwner);
        validatorRegistry.transferOwnership(proxyAdminAddress);

        // Accept ownership as proxy admin
        vm.prank(proxyAdminAddress);
        validatorRegistry.acceptOwnership();

        // Prepare initialization data for registerValidator (called by new owner = proxyAdminAddress)
        bytes memory initData =
            abi.encodeWithSignature("registerValidator(bytes,uint64)", validator3PublicKey, VOTING_POWER_3);

        // Perform upgrade with initialization
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall(address(newImplementation), initData);

        assertEq(
            validatorRegistry.getActiveValidatorsWithPositiveVotingPowerCount(),
            0,
            "Positive active voting power count should remain zero when no validators are active"
        );

        // Verify the upgrade succeeded and initialization was called
        assertEq(
            validatorRegistry.getNextRegistrationId(),
            4,
            "Next registration ID should be 4 after upgrade initialization"
        );

        // Verify the new validator was registered during upgrade
        Validator memory newValidator = validatorRegistry.getValidator(3);
        assertEq(newValidator.publicKey, validator3PublicKey, "New validator should be registered during upgrade");
        assertEq(newValidator.votingPower, VOTING_POWER_3, "New validator voting power should match");
    }

    function test_ValidatorRegistryStatePreservationAcrossUpgrade() public {
        // Register and activate validators, modify state through proxy
        vm.startPrank(registryOwner);
        uint256 regId1 = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);
        uint256 regId2 = validatorRegistry.registerValidator(validator2PublicKey, VOTING_POWER_2);
        validatorRegistry.activateValidator(regId1);
        validatorRegistry.activateValidator(regId2);

        // Update voting power
        validatorRegistry.updateValidatorVotingPower(regId1, 150);
        vm.stopPrank();

        // Verify state before upgrade
        Validator memory validator1Before = validatorRegistry.getValidator(regId1);
        Validator memory validator2Before = validatorRegistry.getValidator(regId2);
        Validator[] memory activeValidatorsBefore = validatorRegistry.getActiveValidatorSet();
        uint256 nextRegIdBefore = validatorRegistry.getNextRegistrationId();

        assertEq(validator1Before.votingPower, 150, "Validator 1 voting power should be updated before upgrade");
        assertEq(
            uint256(validator1Before.status),
            uint256(ValidatorStatus.Active),
            "Validator 1 should be active before upgrade"
        );
        assertEq(
            uint256(validator2Before.status),
            uint256(ValidatorStatus.Active),
            "Validator 2 should be active before upgrade"
        );
        assertEq(activeValidatorsBefore.length, 2, "Should have 2 active validators before upgrade");
        assertEq(nextRegIdBefore, 3, "Next registration ID should be 3 before upgrade");

        // Deploy new implementation and upgrade
        ValidatorRegistry newImplementation = new ValidatorRegistry();
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // Verify state is preserved after upgrade
        Validator memory validator1After = validatorRegistry.getValidator(regId1);
        Validator memory validator2After = validatorRegistry.getValidator(regId2);
        Validator[] memory activeValidatorsAfter = validatorRegistry.getActiveValidatorSet();
        uint256 nextRegIdAfter = validatorRegistry.getNextRegistrationId();

        assertEq(validator1After.votingPower, 150, "Validator 1 voting power should be preserved after upgrade");
        assertEq(
            uint256(validator1After.status),
            uint256(ValidatorStatus.Active),
            "Validator 1 should remain active after upgrade"
        );
        assertEq(
            uint256(validator2After.status),
            uint256(ValidatorStatus.Active),
            "Validator 2 should remain active after upgrade"
        );
        assertEq(validator1After.publicKey, validator1PublicKey, "Validator 1 public key should be preserved");
        assertEq(validator2After.publicKey, validator2PublicKey, "Validator 2 public key should be preserved");
        assertEq(activeValidatorsAfter.length, 2, "Should have 2 active validators after upgrade");
        assertEq(nextRegIdAfter, 3, "Next registration ID should be preserved after upgrade");

        vm.startPrank(registryOwner);
        validatorRegistry.removeValidator(regId1);
        Validator[] memory activeValidatorsAfterRemoval = validatorRegistry.getActiveValidatorSet();
        assertEq(activeValidatorsAfterRemoval.length, 1, "Should have 1 active validator after removing one");

        vm.expectRevert(ValidatorRegistry.InvalidValidatorSet.selector);
        validatorRegistry.removeValidator(regId2);
        vm.stopPrank();
    }

    // ============ ValidatorRegistry-Specific Edge Cases ============

    function test_MultipleValidatorRegistryProxies() public {
        // Deploy another proxy with the same ValidatorRegistry implementation
        AdminUpgradeableProxy proxy2 = new AdminUpgradeableProxy(address(implementation), proxyAdminAddress, "");

        ValidatorRegistry validatorRegistry2 = ValidatorRegistry(address(proxy2));

        // Initialize second proxy with different owner
        address registryOwner2 = makeAddr("registryOwner2");

        // Set owner for second proxy
        bytes32 ownableStorageSlot =
            keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff));
        vm.store(address(validatorRegistry2), ownableStorageSlot, bytes32(uint256(uint160(registryOwner2))));

        // Initialize _nextRegistrationId to 1 for second proxy
        vm.store(
            address(validatorRegistry2), bytes32(uint256(VALIDATOR_REGISTRY_STORAGE_LOCATION) + 4), bytes32(uint256(1))
        );

        // Register different validators in each proxy
        vm.prank(registryOwner);
        uint256 regId1Proxy1 = validatorRegistry.registerValidator(validator1PublicKey, VOTING_POWER_1);

        vm.prank(registryOwner2);
        uint256 regId1Proxy2 = validatorRegistry2.registerValidator(validator2PublicKey, VOTING_POWER_2);

        // Verify they have independent state
        assertEq(regId1Proxy1, 1, "First proxy should have registration ID 1");
        assertEq(regId1Proxy2, 1, "Second proxy should also have registration ID 1");

        Validator memory validator1Proxy1 = validatorRegistry.getValidator(regId1Proxy1);
        Validator memory validator1Proxy2 = validatorRegistry2.getValidator(regId1Proxy2);

        assertEq(validator1Proxy1.publicKey, validator1PublicKey, "First proxy should have validator1 key");
        assertEq(validator1Proxy2.publicKey, validator2PublicKey, "Second proxy should have validator2 key");
        assertEq(validator1Proxy1.votingPower, VOTING_POWER_1, "First proxy should have voting power 1");
        assertEq(validator1Proxy2.votingPower, VOTING_POWER_2, "Second proxy should have voting power 2");

        // Verify they can be upgraded independently
        ValidatorRegistry newImplementation = new ValidatorRegistry();
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // First proxy upgraded, second still on old implementation
        // Both should preserve their independent state
        Validator memory validator1Proxy1AfterUpgrade = validatorRegistry.getValidator(regId1Proxy1);
        Validator memory validator1Proxy2AfterUpgrade = validatorRegistry2.getValidator(regId1Proxy2);

        assertEq(
            validator1Proxy1AfterUpgrade.publicKey,
            validator1PublicKey,
            "First proxy should preserve state after upgrade"
        );
        assertEq(
            validator1Proxy2AfterUpgrade.publicKey,
            validator2PublicKey,
            "Second proxy should preserve independent state"
        );
    }
}
