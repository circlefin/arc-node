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

import {Script, console, console2} from "forge-std/Script.sol";
import {ValidatorRegistry, Validator, ValidatorStatus} from "../src/validator-manager/ValidatorRegistry.sol";
import {PermissionedValidatorManager} from "../src/validator-manager/PermissionedValidatorManager.sol";
import {Addresses} from "./Addresses.sol";

/**
 * @notice Helper script for managing validator registrations, activations, and voting power updates
 * @dev Usage: 
 * 
 * Print the active validator set
 *  forge script script/ValidatorManagement.s.sol --rpc-url <network> --sig "printActiveValidatorSet()"
 * 
 * Register a new validator public key 
 *  forge script scrips/ValidatorManagement.s.sol --rpc-url <network> --sig "registerValidator()"
 * 
 * Configure controller
 *  forge script scrips/ValidatorManagement.s.sol --rpc-url <network> --sig "configureController()"
 * 
 * Activate validator
 *  forge script scrips/ValidatorManagement.s.sol --rpc-url <network> --sig "activateValidator()"
 * 
 * Update voting power (no safety checks)
 *  forge script scrips/ValidatorManagement.s.sol --rpc-url <network> --sig "updateVotingPowerUnsafe(10000)"
 * 
 * Update voting power (with safety checks)
 *  forge script scrips/ValidatorManagement.s.sol --rpc-url <network> --sig "updateVotingPower(10000)"
 */
contract ValidatorManagement is Script {
    
    // ============ Constants ============
    
    ValidatorRegistry VALIDATOR_REGISTRY = ValidatorRegistry(Addresses.VALIDATOR_REGISTRY);
    PermissionedValidatorManager PERMISSIONED_VALIDATOR_MANAGER = PermissionedValidatorManager(Addresses.PERMISSIONED_MANAGER);

    // ============ Helpers ============

    /**
     * @notice Pretty-prints the currently registered validators
     */
    function printActiveValidatorSet() public view returns (Validator[] memory _validators) {
        _validators = VALIDATOR_REGISTRY.getActiveValidatorSet();

        console.log("Active Validators:");
        console.log("Count: ", _validators.length);
        console.log("-------------------------");
        for (uint256 i = 0; i < _validators.length; i++) {
            console.log("PubKey: ", vm.toString(_validators[i].publicKey));
            console.log("Power:  ", _validators[i].votingPower);
            console.log("Status: ", _statusToString(_validators[i].status));
            console.log("-------------------------");
        }
    }

    /**
     * @notice Pretty-prints a validator managed by a controller
     */
    function printValidatorByController(address controller) public view {
        Validator memory _validator = PERMISSIONED_VALIDATOR_MANAGER.getValidator(controller);

        console.log("-------------------------");
        console.log("PubKey: ", vm.toString(_validator.publicKey));
        console.log("Power:  ", _validator.votingPower);
        console.log("Status: ", _statusToString(_validator.status));
        console.log("-------------------------");
    }

    /**
     * @notice Pretty-prints a validator managed by a controller
     */
    function printValidatorByID(uint256 _registrationId) public view returns (Validator memory _validator) {
        _validator = VALIDATOR_REGISTRY.getValidator(_registrationId);

        console.log("-------------------------");
        console.log("PubKey: ", vm.toString(_validator.publicKey));
        console.log("Power:  ", _validator.votingPower);
        console.log("Status: ", _statusToString(_validator.status));
        console.log("-------------------------");
    }

    // ============ Registration Flows ============

    /**
     * @notice Register's a new validators public key
     * @dev Requires VALIDATOR_REGISTERER_KEY to be set in the environment
     * @dev Requires VALIDATOR_PUBLIC_KEY_BYTES to be set in the environment
     */
    function registerValidator() public returns (uint256 _registrationId) {
        uint256 _validatorRegistererKey = vm.envUint(
            "VALIDATOR_REGISTERER_KEY"
        );
        bytes memory _validatorPublicKey = vm.envBytes(
            "VALIDATOR_PUBLIC_KEY_BYTES"
        );

        vm.startBroadcast(_validatorRegistererKey);
        _registrationId = PERMISSIONED_VALIDATOR_MANAGER.registerValidator(_validatorPublicKey);
        vm.stopBroadcast();

        console.log(
            "Registered validator with registrationId:", _registrationId, 
            "and public key:", vm.toString(_validatorPublicKey)
        );
    }


    /**
     * @notice Configure a controller to manage a validator registrationId
     * @dev Requires CONTROLLER_ADDRESS to be set in the environment
     * @dev Requires REGISTRATION_ID to be set in the environment
     * @dev Requires CONTROLLER_VOTING_POWER_LIMIT to be set in the environment
     * @dev Requires PERMISSIONED_VALIDATOR_MANAGER_OWNER to be set in the environment
     */
    function configureController() public {
        address _controller = vm.envAddress(
            "CONTROLLER_ADDRESS"
        );
        uint256 _registrationId = vm.envUint(
            "REGISTRATION_ID"
        );
        uint64 _maxVotingPower = uint64(vm.envUint("CONTROLLER_VOTING_POWER_LIMIT"));
        uint256 _permissionedOwnerKey = vm.envUint(
            "PERMISSIONED_VALIDATOR_MANAGER_OWNER"
        );

        // Broadcast update
        vm.startBroadcast(_permissionedOwnerKey);
        PERMISSIONED_VALIDATOR_MANAGER.configureController(_controller, _registrationId, _maxVotingPower);
        vm.stopBroadcast();

        // Log configuration parameters for visibility
        console2.log("Configure controller");
        console2.log("controller", _controller);
        console2.log("registrationId", _registrationId);
        console2.log("maxVotingPower", uint256(_maxVotingPower));
    }

    /**
     * @notice Activates a new validator, using its controller
     * @dev Requires CONTROLLER_KEY to be set in the environment
     */
    function activateValidator() public {
        uint256 _controllerKey = vm.envUint(
            "CONTROLLER_KEY"
        );

        // Sanity check: validator voting power should be 0
        Validator memory _validator = PERMISSIONED_VALIDATOR_MANAGER.getValidator(vm.addr(_controllerKey));
        require(_validator.votingPower == 0, "Validator voting power should be 0");

        // Activate the validator 
        vm.startBroadcast(_controllerKey);
        PERMISSIONED_VALIDATOR_MANAGER.activateValidator();
        vm.stopBroadcast();
    }

    /**
     * @notice Updates the voting power of a validator, using its controller
     * @dev Requires CONTROLLER_KEY to be set in the environment
     * @dev Enforces invariants that no validator can have critical voting power after update
     */
    function updateVotingPower(uint64 _newVotingPower) public {
        uint256 _controllerKey = vm.envUint(
            "CONTROLLER_KEY"
        );
        address _controllerAddress = vm.addr(_controllerKey);
        Validator memory _validator = _checkControllerForVotingPowerUpdate(_controllerAddress);

        // Update the voting power
        // Sanity check: make sure the validator mutation does not give it 
        // or another validator more than 1/3 of the total voting power
        Validator[] memory _validators = VALIDATOR_REGISTRY.getActiveValidatorSet();

        uint256 _totalVotingPower = 0;
        uint256 _highestVotingPower = 0;
        for (uint256 i = 0; i < _validators.length; i++) {
            // Simulate as if the validator was updated
            if (keccak256(_validators[i].publicKey) == keccak256(_validator.publicKey)) {
                _validators[i].votingPower = _newVotingPower;
            }

            // Record running highest voting power
            if (_validators[i].votingPower > _highestVotingPower) {
                _highestVotingPower = _validators[i].votingPower;
            }
            _totalVotingPower += _validators[i].votingPower;
        }

        // Enforce invariant check
        require(_highestVotingPower * 3 < _totalVotingPower, "Highest voting power exceeds 1/3 of total voting power");

        // Broadcast update
        vm.startBroadcast(_controllerKey);
        PERMISSIONED_VALIDATOR_MANAGER.updateValidatorVotingPower(_newVotingPower);
        vm.stopBroadcast();

        console.log("Voting power updated:", _newVotingPower);
    }

    /**
     * @notice Updates the voting power of a validator, using its controller
     * @dev Requires CONTROLLER_KEY to be set in the environment
     * @dev WARNING: does not enforce any invariant checks
     */
    function updateVotingPowerUnsafe(uint64 _newVotingPower) public {
        uint256 _controllerKey = vm.envUint(
            "CONTROLLER_KEY"
        );
        address _controllerAddress = vm.addr(_controllerKey);
        _checkControllerForVotingPowerUpdate(_controllerAddress);

          // Broadcast update
        vm.startBroadcast(_controllerKey);
        PERMISSIONED_VALIDATOR_MANAGER.updateValidatorVotingPower(_newVotingPower);
        vm.stopBroadcast();

        console.log("Voting power updated:", _newVotingPower);
    }

    // ============ Internal Utils ============

    /**
     * @notice Sanity-checks that a controller is valid for a voting power update
     */
    function _checkControllerForVotingPowerUpdate(address _controllerAddress) internal view returns (Validator memory _validator) {
        _validator = PERMISSIONED_VALIDATOR_MANAGER.getValidator(_controllerAddress);
        require(_validator.status == ValidatorStatus.Active || _validator.status == ValidatorStatus.Registered, "Validator not active or registered");
        uint256 _registrationId = PERMISSIONED_VALIDATOR_MANAGER.getRegistrationId(_controllerAddress);
        require(_registrationId != 0, "RegistrationId not found");
    }

    /**
     * @notice String representation of a ValidatorStatus case
     */
    function _statusToString(ValidatorStatus status) internal pure returns (string memory) {
        if (status == ValidatorStatus.Unknown) return "Unknown";
        if (status == ValidatorStatus.Registered) return "Registered";
        if (status == ValidatorStatus.Active) return "Active";
        return "Invalid";
    }
}
