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
import {ProtocolConfig} from "../src/protocol-config/ProtocolConfig.sol";

/**
 * @notice Script to deploy a new ProtocolConfig implementation
 * @dev This script deploys the implementation contract only (not the proxy)
 *
 * Usage:
 *   forge script contracts/scripts/DeployProtocolConfig.s.sol \
 *     --rpc-url <network> \
 *     --broadcast \
 *     --verify
 *
 * Environment Variables:
 *   DEPLOYER_PRIVATE_KEY - Private key of the deployer account (required)
 */
contract DeployProtocolConfig is Script {
    function run() external returns (address implementation) {
        uint256 deployerKey = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(deployerKey);

        console.log("Deployer:", deployer);

        vm.startBroadcast(deployerKey);

        // Deploy new ProtocolConfig implementation
        ProtocolConfig protocolConfig = new ProtocolConfig();
        implementation = address(protocolConfig);

        vm.stopBroadcast();

        // Verify deployment
        require(implementation.code.length > 0, "Deployment failed: no bytecode");

        console.log("Implementation:", implementation);

        return implementation;
    }
}

