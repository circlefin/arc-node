// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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
import {Memo} from "../src/memo/Memo.sol";

/**
 * @notice Script to deploy Memo
 * @dev Not upgradeable, no proxy. Deployed via CREATE2 for deterministic addressing.
 *
 * Usage:
 *   forge script contracts/scripts/DeployMemo.s.sol \
 *     --rpc-url <network> \
 *     --broadcast \
 *     --verify
 *
 * Environment Variables:
 *   DEPLOYER_KEY - Private key of the deployer account (required)
 */
contract DeployMemo is Script {
    function run() external returns (address deployment) {
        uint256 deployerKey = vm.envUint("DEPLOYER_KEY");
        address deployer = vm.addr(deployerKey);

        console.log("Deployer:", deployer);

        vm.startBroadcast(deployerKey);

        Memo memoContract = new Memo{salt: bytes32(0)}();
        deployment = address(memoContract);

        vm.stopBroadcast();

        require(deployment.code.length > 0, "Deployment failed: no bytecode");

        console.log("Memo:", deployment);

        return deployment;
    }
}
