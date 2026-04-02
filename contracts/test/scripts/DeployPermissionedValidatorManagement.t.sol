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
import {DeployPermissionedValidatorManager} from "../../scripts/DeployPermissionedValidatorManager.s.sol";
import {PermissionedValidatorManager} from "../../src/validator-manager/PermissionedValidatorManager.sol";
import {Addresses} from "../../scripts/Addresses.sol";

/**
 * @title DeployPermissionedValidatorManagementTest
 * @notice Test suite for DeployPermissionedValidatorManager deployment script
 * @dev Tests the pure deploy() function with random inputs - no mocking needed!
 */
contract DeployPermissionedValidatorManagementTest is Test {
    DeployPermissionedValidatorManager deployScript;
    
    function setUp() public {
        deployScript = new DeployPermissionedValidatorManager();
    }

    
    function test_Deploy_WithSpecificAddresses() public {
        address testOwner = address(0x123);
        address testPauser = address(0x789);
        address testAdmin = address(0x456);
        
        (address implAddr, address proxyAddr) = deployScript.deploy(testOwner, testPauser, testAdmin);
        
        assertTrue(implAddr != address(0));
        assertTrue(proxyAddr != address(0));
        
        PermissionedValidatorManager pvm = PermissionedValidatorManager(proxyAddr);
        assertEq(pvm.owner(), testOwner);
        assertEq(pvm.pauser(), testPauser);
        assertEq(address(pvm.REGISTRY()), Addresses.VALIDATOR_REGISTRY);
    }
}
