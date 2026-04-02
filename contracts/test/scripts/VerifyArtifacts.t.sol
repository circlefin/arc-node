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

import {ArtifactHelper} from "../../scripts/ArtifactHelper.s.sol";

contract VerifyArtifactTest is ArtifactHelper, Test {
    uint256 ethereumFork;
    bool verifyEthCodeHash = false;

    function setUp() public {
        loadManifest("assets/artifacts/manifest.json");

        string memory ethRpcUrl = vm.envOr("ETH_RPC_URL", string(""));
        if (bytes(ethRpcUrl).length > 0) {
            ethereumFork = vm.createFork(ethRpcUrl);
            verifyEthCodeHash = true;
        }
        // To compare the codeHash with Ethereum mainnet, we need to set the chainId to 1.
        vm.chainId(1);
    }

    function getEthCodeHash(address addr) internal returns (bytes32) {
        // Same as `cast codehash --rpc-url https://reth-ethereum.ithaca.xyz/rpc $addr`
        vm.selectFork(ethereumFork);
        string memory params = string.concat("[\"", vm.toString(addr), "\"]");
        bytes memory code = vm.rpc("eth_getCode", params);
        return keccak256(code);
    }

    function verifyDeterministicDeployedContract(DeterministicDeployment memory deployment) public {
        address addr = deployDeterministicContract(deployment);

        bytes32 codeHash = keccak256(addr.code);
        assertEq(codeHash, deployment.ethCodeHash, "deployed codehash mismatch");
    }

    function verifyOneTimeAddressContract(OneTimeAddressDeployment memory deployment) public {
        // Forge env already provide same determinisic deployment proxy, remove it to deploy again.
        vm.etch(deployment.addr, hex"");
        vm.resetNonce(deployment.addr);
        vm.resetNonce(deployment.deployer);

        vm.deal(deployment.deployer, deployment.deployerBalance);
        vm.broadcastRawTransaction(deployment.rawTransaction);

        bytes32 codeHash = keccak256(deployment.addr.code);
        assertEq(codeHash, deployment.ethCodeHash, "deployed codehash mismatch");
    }

    // TODO we could move to table test after foundry 1.3.
    function testVerifyArtifacts() public {
        for (uint256 i = 0; i < externalContracts.length; i++) {
            string memory contractName = externalContracts[i];
            DeploymentType deploymentType = getDeploymentType(contractName);

            if (deploymentType == DeploymentType.deterministic) {
                DeterministicDeployment memory deployment = loadDeterministicDeployment(contractName);
                verifyDeterministicDeployedContract(deployment);
            } else {
                OneTimeAddressDeployment memory deployment = loadOneTimeAddressDeployment(contractName);
                verifyOneTimeAddressContract(deployment);
            }
        }
    }

    function testEthCodeHash() public {
        vm.skip(!verifyEthCodeHash);

        for (uint256 i = 0; i < externalContracts.length; i++) {
            string memory contractName = externalContracts[i];
            DeploymentType deploymentType = getDeploymentType(contractName);

            if (deploymentType == DeploymentType.deterministic) {
                DeterministicDeployment memory deployment = loadDeterministicDeployment(contractName);
                bytes32 ethCodeHash = getEthCodeHash(deployment.addr);
                assertEq(ethCodeHash, deployment.addr.codehash, "code hash mismatch with Ethereum mainnet");
            } else {
                OneTimeAddressDeployment memory deployment = loadOneTimeAddressDeployment(contractName);
                bytes32 ethCodeHash = getEthCodeHash(deployment.addr);
                assertEq(ethCodeHash, deployment.addr.codehash, "code hash mismatch with Ethereum mainnet");
            }
        }
    }

    function testDeterministicDeploymentProxyAddress() public view {
        OneTimeAddressDeployment memory deployment = loadOneTimeAddressDeployment("DeterministicDeploymentProxy");
        assertEq(deployment.addr, DETERMINISTIC_DEPLOYMENT_PROXY, "deterministic deployment proxy address mismatch");
    }

    function testPermit2DomainSeparator() public {
        DeterministicDeployment memory deployment = loadDeterministicDeployment("Permit2");

        vm.chainId(1);
        bytes32 domainSeparator = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,uint256 chainId,address verifyingContract)"),
                keccak256("Permit2"),
                block.chainid,
                deployment.addr
            )
        );
        assertEq(block.chainid, 1, "chain ID mismatch");
        assertEq(
            domainSeparator,
            bytes32(0x866a5aba21966af95d6c7ab78eb2b2fc913915c28be3b9aa07cc04ff903e3f28),
            "domain separator mismatch"
        );
    }
}
