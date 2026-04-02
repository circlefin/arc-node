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

import {Script, console} from "forge-std/Script.sol";
import {PermissionedValidatorManager, IValidatorRegistry} from "../src/validator-manager/PermissionedValidatorManager.sol";
import {AdminUpgradeableProxy} from "../src/proxy/AdminUpgradeableProxy.sol";
import {Addresses} from "./Addresses.sol";

/**
 * @notice Deploys the PermissionedValidatorManager contract
 * command: DEPLOYER_KEY=$DEPLOYER_KEY \
            forge script contracts/scripts/DeployPermissionedValidatorManager.s.sol:DeployPermissionedValidatorManager \
            --sig "run()" \
            --rpc-url http://localhost:8545 \
            --broadcast
            Note: Owner and proxy admin automatically read from old PVM at 0x36...03
 */
contract DeployPermissionedValidatorManager is Script {

    // ============ Constants ============

    IValidatorRegistry VALIDATOR_REGISTRY = IValidatorRegistry(Addresses.VALIDATOR_REGISTRY);

    /**
     * @notice Read configuration from old PVM
     * @return owner The owner from old PVM
     * @return pauser The original pauser from old PVM
     * @return proxyAdmin The proxy admin from old PVM
     */
    function readOldPVMConfig() public view returns (address owner, address pauser, address proxyAdmin) {
        PermissionedValidatorManager oldPvm = PermissionedValidatorManager(Addresses.PERMISSIONED_MANAGER);
        owner = oldPvm.owner();
        pauser = oldPvm.pauser();
        proxyAdmin = AdminUpgradeableProxy(payable(Addresses.PERMISSIONED_MANAGER)).admin();
    }

    /**
     * @notice Deploy PVM with given configuration
     * @param owner Owner address for new PVM
     * @param pauser Pauser address for new PVM
     * @param proxyAdmin Proxy admin address for new PVM
     * @return implAddress Deployed implementation address
     * @return proxyAddress Deployed proxy address
     */
    function deploy(address owner, address pauser, address proxyAdmin) public returns (address implAddress, address proxyAddress) {
        console.log("=== Deploy PermissionedValidatorManager ===");
        console.log("Owner:", owner);
        console.log("Pauser:", pauser);
        console.log("Proxy Admin:", proxyAdmin);
        console.log("ValidatorRegistry:", Addresses.VALIDATOR_REGISTRY);
        console.log("");
                
        // Deploy implementation
        console.log("Deploying implementation...");
        PermissionedValidatorManager impl = new PermissionedValidatorManager(
            IValidatorRegistry(Addresses.VALIDATOR_REGISTRY)
        );
        implAddress = address(impl);
        console.log("Implementation:", implAddress);
        console.log("");
        
        // Deploy proxy with initialization
        console.log("Deploying proxy...");
        bytes memory initData = abi.encodeWithSignature("initialize(address,address)", owner, pauser);
        AdminUpgradeableProxy proxy = new AdminUpgradeableProxy(
            implAddress,
            proxyAdmin,
            initData
        );
        proxyAddress = address(proxy);
        console.log("Proxy:", proxyAddress);
        console.log("");
        
        console.log("DEPLOYMENT COMPLETE");
    }

    /**
     * @notice Full deployment flow (read config + deploy)
     * @dev Expects DEPLOYER_KEY environment variable
     */
    function run() public returns (address implAddress, address proxyAddress) {
        uint256 deployerKey = vm.envUint("DEPLOYER_KEY");
        
        // Step 1: Read configuration from old PVM
        (address owner, address pauser, address proxyAdmin) = readOldPVMConfig();
        
        // Step 2: Deploy with configuration
        vm.startBroadcast(deployerKey);
        (implAddress, proxyAddress) = deploy(owner, pauser, proxyAdmin);
        vm.stopBroadcast();
    }
}
