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
import {Ownable2StepUpgradeable} from "@openzeppelin/contracts-upgradeable/access/Ownable2StepUpgradeable.sol";
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
     *
     * Runs a cheap sanity check (length, not all-zero) before broadcasting. Full ed25519
     * curve-point validation cannot be done on-chain, and a bad key on-chain forces the
     * consensus layer to skip the affected validator — reducing BFT security, and halting
     * consensus entirely if the surviving validators no longer hold enough voting power
     * for quorum. Operators should additionally cross-check VALIDATOR_PUBLIC_KEY_BYTES
     * with off-chain tooling before running this script.
     */
    function registerValidator() public returns (uint256 _registrationId) {
        uint256 _validatorRegistererKey = vm.envUint(
            "VALIDATOR_REGISTERER_KEY"
        );
        bytes memory _validatorPublicKey = vm.envBytes(
            "VALIDATOR_PUBLIC_KEY_BYTES"
        );

        requirePublicKeyBasicSanity(_validatorPublicKey);

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
     * @notice Cheap off-chain sanity check on an ed25519 public key.
     * @dev Catches obvious operator mistakes (wrong length, zero/placeholder bytes) before
     *      they reach the registry. Full curve-point validation requires ed25519 math that
     *      is not feasible on-chain; operators must validate with off-chain tooling as well.
     *      Public (rather than internal) to allow direct unit testing without going through
     *      environment-variable plumbing.
     */
    function requirePublicKeyBasicSanity(bytes memory publicKey) public pure {
        require(publicKey.length == 32, "Public key must be 32 bytes");
        bool allZero = true;
        for (uint256 i = 0; i < publicKey.length; i++) {
            if (publicKey[i] != 0x00) { allZero = false; break; }
        }
        require(!allZero, "Public key must not be all zero");
    }

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

/// @title ValidatorRegistryState
/// @notice Preserved-state hash helper used by upgrade/rollback scripts under
///         `contracts/deployments/<date>-validator-registry-*/scripts/`.
///
///         Aggregates every field an implementation-only upgrade must preserve and returns a
///         single hash. Pre-boundary and post-boundary calls should produce equal hashes; any
///         divergence indicates a storage-slot collision, accidental overwrite, or layout drift
///         between old and new implementations.
///
///         Validity condition: valid only when the `Validator` struct layout returned by
///         `getValidator` and `getActiveValidatorSet` is unchanged between old and new impl.
///         A struct layout change would make `abi.encode` produce different bytes for the same
///         logical state — when that happens, replace this helper with field-by-field comparison
///         on the surviving fields.
library ValidatorRegistryState {
    function hash(address proxy) internal view returns (bytes32) {
        ValidatorRegistry vr = ValidatorRegistry(proxy);
        uint256 nextId = vr.getNextRegistrationId();

        bytes32[] memory perValidator = new bytes32[](nextId);
        for (uint256 i = 0; i < nextId; ++i) {
            Validator memory v = vr.getValidator(i);
            perValidator[i] = keccak256(abi.encode(v.status, v.publicKey, v.votingPower));
        }

        return keccak256(
            abi.encode(
                Ownable2StepUpgradeable(proxy).owner(),
                Ownable2StepUpgradeable(proxy).pendingOwner(),
                nextId,
                vr.getActiveValidatorSet(),
                perValidator
            )
        );
    }
}
