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

import {Validator} from "./IValidatorRegistry.sol";

/**
 * @dev PermissionedValidatorManager interface meant to be used as the owner of an
 * {IValidatorRegistry} contract instance that only allows modifications to the validator set
 * to be initiated by a fixed address.
 */
interface IPermissionedValidatorManager {
    /**
     * @notice Register validator.
     * @param publicKey The Ed25519 public key of the validator.
     * @return registrationId The unique identifier for the validator registration.
     */
    function registerValidator(bytes memory publicKey) external returns (uint256 registrationId);

    /**
     * @notice Activate validator. Only callable by the controller.
     */
    function activateValidator() external;

    /**
     * @notice Remove validator. Only callable by the controller.
     */
    function removeValidator() external;

    /**
     * @notice Update a validator voting power. Only callable by the controller.
     * @param newVotingPower The new voting power for the validator.
     */
    function updateValidatorVotingPower(uint64 newVotingPower) external;

    /**
     * @notice Get validator information for the specified controller.
     * @param controller The controller address to get validator information for.
     * @return validator The validator struct containing status, public key, and voting power.
     */
    function getValidator(address controller) external view returns (Validator memory validator);
}
