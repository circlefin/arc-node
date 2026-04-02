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

import {TestUtils} from "../validator-manager/TestUtils.sol";
import {ValidatorRegistry, Validator} from "../../src/validator-manager/ValidatorRegistry.sol";
import {PermissionedValidatorManager} from "../../src/validator-manager/PermissionedValidatorManager.sol";
import {ValidatorManagement} from "../../scripts/ValidatorManagement.s.sol";

contract ValidatorManagementTest is TestUtils {
    ValidatorRegistry constant VALIDATOR_REGISTRY = ValidatorRegistry(0x3600000000000000000000000000000000000002);
    PermissionedValidatorManager constant PERMISSIONED_VALIDATOR_MANAGER =
        PermissionedValidatorManager(0x3600000000000000000000000000000000000003);

    ValidatorManagement validatorManagement;

    uint256 ownerPk = uint256(keccak256("OWNER_PK"));
    address owner = vm.addr(ownerPk);
    uint256 validatorRegistererPk = uint256(keccak256("VALIDATOR_REGISTERER_PK"));
    address validatorRegisterer = vm.addr(validatorRegistererPk);
    address initializedController = address(30);
    // Use a high limit to allow tests (including unsafe update) to set large powers
    uint64 controllerVotingPowerLimit = type(uint64).max;

    uint256 newControllerPk = uint256(keccak256("CONTROLLER_PK"));
    address newController = vm.addr(newControllerPk);

    // Initialized validators
    bytes validator1PublicKey = generateEd25519PublicKey(10); // 80 voting power
    bytes validator2PublicKey = generateEd25519PublicKey(20); // 90 voting power
    bytes validator3PublicKey = generateEd25519PublicKey(30); // 100 voting power

    // New validator
    bytes newValidatorPublicKey = generateEd25519PublicKey(40);

    function setUp() public {
        // Deploy contracts at temporary addresses first
        ValidatorRegistry validatorRegistryImpl = new ValidatorRegistry();

        // Use etch to move the ValidatorRegistry bytecode to expected address first
        vm.etch(0x3600000000000000000000000000000000000002, address(validatorRegistryImpl).code);

        // Now deploy PermissionedValidatorManager with the correct registry address
        PermissionedValidatorManager permissionedValidatorManagerImpl =
            new PermissionedValidatorManager(VALIDATOR_REGISTRY);
        
        validatorManagement = new ValidatorManagement();

        // Use etch to move PermissionedValidatorManager bytecode to expected address
        vm.etch(0x3600000000000000000000000000000000000003, address(permissionedValidatorManagerImpl).code);
        
        // Set Ownable slots (simulating genesis initialization)
        bytes32 ownableSlot = 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300;
        // Set ValidatorRegistry owner to PERMISSIONED_VALIDATOR_MANAGER
        vm.store(
            address(VALIDATOR_REGISTRY), 
            ownableSlot, 
            bytes32(uint256(uint160(address(PERMISSIONED_VALIDATOR_MANAGER))))
        );
        // Set PermissionedValidatorManager owner to owner
        vm.store(
            address(PERMISSIONED_VALIDATOR_MANAGER), 
            ownableSlot, 
            bytes32(uint256(uint160(owner)))
        );
        // Initialize ValidatorRegistry registrationId storage to start at 1
        bytes32 nextRegistrationIdSlot = bytes32(uint256(VALIDATOR_REGISTRY.VALIDATOR_REGISTRY_STORAGE_LOCATION()) + 4);
        vm.store(address(VALIDATOR_REGISTRY), nextRegistrationIdSlot, bytes32(uint256(1)));

        // Configure some initial state
        // 3 validators
        vm.prank(owner);
        PERMISSIONED_VALIDATOR_MANAGER.addValidatorRegisterer(validatorRegisterer);

        // Register keys
        vm.startPrank(validatorRegisterer);
        PERMISSIONED_VALIDATOR_MANAGER.registerValidator(validator1PublicKey);
        PERMISSIONED_VALIDATOR_MANAGER.registerValidator(validator2PublicKey);
        PERMISSIONED_VALIDATOR_MANAGER.registerValidator(validator3PublicKey);
        vm.stopPrank();

        // Set voting power and activate each validator
        uint64[3] memory powers = [uint64(80), uint64(90), uint64(100)];
        for (uint256 i = 1; i <= 3; i++) {
            vm.prank(owner);
            PERMISSIONED_VALIDATOR_MANAGER.configureController(initializedController, i, controllerVotingPowerLimit);

            vm.startPrank(initializedController);
            PERMISSIONED_VALIDATOR_MANAGER.updateValidatorVotingPower(powers[i - 1]);
            PERMISSIONED_VALIDATOR_MANAGER.activateValidator();
            vm.stopPrank();

            vm.prank(owner);
            PERMISSIONED_VALIDATOR_MANAGER.removeController(initializedController);
        }
    }

    function test_PrintActiveValidatorSet() public view {
        Validator[] memory _validators = validatorManagement.printActiveValidatorSet();

        assertEq(_validators.length, 3);
        assertEq(_validators[0].publicKey, validator1PublicKey);
        assertEq(_validators[1].publicKey, validator2PublicKey);
        assertEq(_validators[2].publicKey, validator3PublicKey);

        assertEq(_validators[0].votingPower, 80);
        assertEq(_validators[1].votingPower, 90);
        assertEq(_validators[2].votingPower, 100);

        assertEq(uint256(_validators[0].status), 2, "Status should be Active");
        assertEq(uint256(_validators[1].status), 2, "Status should be Active");
        assertEq(uint256(_validators[2].status), 2, "Status should be Active");
    }

    function test_RegisterValidator_Succeeds() public {
        uint256 _registrationId = _registerValidator();

        // Retrieve validator, and confirm that 0 voting power was registered
        Validator memory _validator = VALIDATOR_REGISTRY.getValidator(_registrationId);
        assertEq(_validator.votingPower, 0); // Voting power should be 0
        assertEq(_validator.publicKey, newValidatorPublicKey);
        assertEq(uint256(_validator.status), 1); // Registered
    }

    function test_ConfigureController_Succeeds() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);

        // Verify controller is configured
        assertTrue(PERMISSIONED_VALIDATOR_MANAGER.isController(newController), "Controller should be configured");
    }

    function test_ActivateValidator_Succeeds() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);
        _activateValidator();

        // Verify validator is active
        Validator memory _validator = VALIDATOR_REGISTRY.getValidator(_registrationId);
        assertEq(uint256(_validator.status), 2, "Validator should be active");
        assertEq(_validator.publicKey, newValidatorPublicKey);
    }

    function test_UpdateVotingPower_Succeeds() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);
        _activateValidator();

        // Registry is initialized with 80 + 90 + 100 = 270 voting power
        // X < (270 + X) / 3
        // 3X < 270 + X
        // 2X < 270
        // --> X < 135
        _updateVotingPower(134);

        // Verify validator voting power is updated
        Validator memory _validator = VALIDATOR_REGISTRY.getValidator(_registrationId);
        assertEq(_validator.votingPower, 134);
    }

    function test_UpdateVotingPower_FailsIfNewValidatorPowerIsCritical() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);
        _activateValidator();

        vm.expectRevert("Highest voting power exceeds 1/3 of total voting power");
        _updateVotingPower(135); // See comment in test above
    }

    function test_UpdateVotingPower_FailsIfExistingValidatorPowerIsCritical() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);
        _activateValidator();

        // 80 + 90 + 100 = 270 total voting power
        // Add a nominal power to the new validator ==> should fail, since the existing validators
        // are considered critical
        vm.expectRevert("Highest voting power exceeds 1/3 of total voting power");
        _updateVotingPower(10); // See comment in test above
    }

    function test_UpdateVotingPowerUnsafe_Succeeds() public {
        uint256 _registrationId = _registerValidator();
        _configureController(_registrationId);
        _activateValidator();

        _updateVotingPowerUnsafe(10000);

        // Verify validator voting power is updated
        Validator memory _validator = VALIDATOR_REGISTRY.getValidator(_registrationId);
        assertEq(_validator.votingPower, 10000);
    }

    // Internal helpers

    function _registerValidator() internal returns (uint256 _registrationId) {
        vm.setEnv("VALIDATOR_REGISTERER_KEY", vm.toString(validatorRegistererPk));
        vm.setEnv("VALIDATOR_PUBLIC_KEY_BYTES", vm.toString(newValidatorPublicKey));
        _registrationId = validatorManagement.registerValidator();
    }

    function _configureController(uint256 _registrationId) internal {
        vm.setEnv("CONTROLLER_ADDRESS", vm.toString(newController));
        vm.setEnv("REGISTRATION_ID", vm.toString(_registrationId));
        vm.setEnv("PERMISSIONED_VALIDATOR_MANAGER_OWNER", vm.toString(ownerPk));
        vm.setEnv("CONTROLLER_VOTING_POWER_LIMIT", vm.toString(controllerVotingPowerLimit));

        validatorManagement.configureController();
    }

    function _activateValidator() internal {
        vm.setEnv("CONTROLLER_KEY", vm.toString(newControllerPk));

        validatorManagement.activateValidator();
    }

    function _updateVotingPower(uint64 _newVotingPower) internal {
        vm.setEnv("CONTROLLER_KEY", vm.toString(newControllerPk));

        validatorManagement.updateVotingPower(_newVotingPower);
    }

    function _updateVotingPowerUnsafe(uint64 _newVotingPower) internal {
        vm.setEnv("CONTROLLER_KEY", vm.toString(newControllerPk));

        validatorManagement.updateVotingPowerUnsafe(_newVotingPower);
    }
}
