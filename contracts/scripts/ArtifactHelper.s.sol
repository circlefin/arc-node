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
import {stdJson} from "forge-std/StdJson.sol";

import {AdminUpgradeableProxy} from "../src/proxy/AdminUpgradeableProxy.sol";
import {ProtocolConfig} from "../src/protocol-config/ProtocolConfig.sol";
import {ValidatorRegistry} from "../src/validator-manager/ValidatorRegistry.sol";
import {PermissionedValidatorManager} from "../src/validator-manager/PermissionedValidatorManager.sol";

contract ArtifactHelper is Script {
  using stdJson for string;

  address constant DETERMINISTIC_DEPLOYMENT_PROXY = address(0x4e59b44847b379578588920cA78FbF26c0B4956C);
  string manifest;
  string[] externalContracts = [
    "DeterministicDeploymentProxy",
    "Multicall3",
    "BlockHashHistory",
    "Permit2"
  ];

  enum DeploymentType {
    deterministic,
    oneTimeAddress
  }

  function loadManifest(string memory path) public returns (string memory) {
    manifest = vm.readFile(string.concat(vm.projectRoot(), "/", path));
    return manifest;
  }

  function getDeploymentType(string memory contractName) public view returns (DeploymentType) {
    string memory prefix = string.concat(".", contractName);
    string memory typeStr = vm.parseJsonString(manifest, string.concat(prefix, ".type"));
    if (keccak256(bytes(typeStr)) == keccak256(bytes("deterministic"))) {
      return DeploymentType.deterministic;
    } else {
      return DeploymentType.oneTimeAddress;
    }
  }

  struct OneTimeAddressDeployment {
    address deployer;
    uint256 deployerBalance;
    bytes rawTransaction;
    address addr;
    bytes32 ethCodeHash;
  }

  function loadOneTimeAddressDeployment(string memory contractName) public view returns (OneTimeAddressDeployment memory) {
    OneTimeAddressDeployment memory deployment;
    string memory prefix = string.concat(".", contractName);
    deployment.addr = manifest.readAddress(string.concat(prefix, ".address"));
    deployment.deployer = manifest.readAddress(string.concat(prefix, ".deployer"));
    deployment.deployerBalance = manifest.readUint(string.concat(prefix, ".deployerBalance"));
    deployment.rawTransaction = manifest.readBytes(string.concat(prefix, ".rawTransaction"));
    deployment.ethCodeHash = manifest.readBytes32(string.concat(prefix, ".ethCodeHash"));
    return deployment;
  }

  struct DeterministicDeployment {
    address addr;
    bytes32 salt;
    bytes32 ethCodeHash;
    bytes bytecode;
  }

  function loadBytecode(string memory filePath, string memory selector) public view returns (bytes memory) {
    string memory artifact = vm.readFile(string.concat(vm.projectRoot(), "/", filePath));
    return artifact.readBytes(selector);
  }

  struct LinkReplacement {
    string placeholder;
    address addr;
  }

  function loadBytecode(string memory filePath, string memory selector, LinkReplacement[] memory linkReplacements) public view returns (bytes memory) {
    string memory artifact = vm.readFile(string.concat(vm.projectRoot(), "/", filePath));
    string memory bytecodeStr = artifact.readString(selector);

    for (uint256 i = 0; i < linkReplacements.length; i++) {
      string memory addrHex = vm.toString(linkReplacements[i].addr);
      addrHex = vm.replace(addrHex, "0x", "");
      bytecodeStr = vm.replace(bytecodeStr, linkReplacements[i].placeholder, addrHex);
    }
    bytecodeStr = string.concat("{\"x\":\"", bytecodeStr, "\"}");
    return bytecodeStr.readBytes(".x");
  }

  function loadDeterministicDeployment(string memory contractName) public view returns (DeterministicDeployment memory) {
    DeterministicDeployment memory deployment;
    string memory prefix = string.concat(".", contractName);
    deployment.addr = manifest.readAddress(string.concat(prefix, ".address"));
    deployment.salt = manifest.readBytes32Or(string.concat(prefix, ".salt"), bytes32(0));
    deployment.ethCodeHash = manifest.readBytes32Or(string.concat(prefix, ".ethCodeHash"), bytes32(0));
    string memory filePath = manifest.readString(string.concat(prefix, ".bytecode.file"));
    string memory selector = manifest.readString(string.concat(prefix, ".bytecode.selector"));
    deployment.bytecode = loadBytecode(filePath, selector);
    return deployment;
  }

  function deployDeterministicContract(bytes memory bytecode) public returns (address) {
    bytes32 salt = bytes32(0);
    return deployDeterministicContract(bytecode, salt);
  }

  function deployDeterministicContract(bytes memory bytecode, bytes32 salt) public returns (address) {
    (bool success, bytes memory result) = DETERMINISTIC_DEPLOYMENT_PROXY.call(
      abi.encodePacked(salt, bytecode));
    require(success, "Deployment failed");
    return address(bytes20(result));
  }

  function deployDeterministicContract(DeterministicDeployment memory deployment) public returns (address) {
    address deployedAddr = deployDeterministicContract(deployment.bytecode, deployment.salt);
    require(deployedAddr == deployment.addr, "deployed address mismatch");
    return deployment.addr;
  }

  function deployOneTimeAddressContract(OneTimeAddressDeployment memory deployment) public returns (address) {
    // Forge env already provide same determinisic deployment proxy, remove it to deploy again.
    vm.etch(deployment.addr, hex"");
    vm.resetNonce(deployment.addr);
    vm.resetNonce(deployment.deployer);

    vm.deal(deployment.deployer, deployment.deployerBalance);
    vm.broadcastRawTransaction(deployment.rawTransaction);

    require(deployment.addr.code.length > 0, "deployed code length mismatch");
    return deployment.addr;
  }

  function getJsonContractCode(address contractAddr) internal returns (string memory) {
    vm.serializeString("forgeContractCode", "address", vm.toString(contractAddr));
    return vm.serializeString("forgeContractCode", "code", vm.toString(contractAddr.code));
  }

  function deployArcNetworkContracts(string memory arcNetworkContractDir, address validatorRegistryProxyAddr) internal returns (string memory) {
    // ProtocolConfig
    address protocolConfig = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "protocol-config/ProtocolConfig.sol/ProtocolConfig.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "ProtocolConfig", getJsonContractCode(address(protocolConfig)));

    // Denylist
    address denylist = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "Denylist.sol/Denylist.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "Denylist", getJsonContractCode(address(denylist)));

    // ValidatorRegistry
    address validatorRegistry = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "validator-manager/ValidatorRegistry.sol/ValidatorRegistry.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "ValidatorRegistry", getJsonContractCode(address(validatorRegistry)));

    // AdminUpgradeableProxy
    address proxy = deployDeterministicContract(
      bytes.concat(
        loadBytecode(
          string.concat(arcNetworkContractDir, "proxy/AdminUpgradeableProxy.sol/AdminUpgradeableProxy.json"),
          ".bytecode"
        ),
        abi.encode(address(validatorRegistry), address(0x0000000000000000000000000000000000000001), hex"")
      )
    );
    vm.serializeString("output", "AdminUpgradeableProxy", getJsonContractCode(address(proxy)));

    // PermissionedValidatorManager
    address poaManager = deployDeterministicContract(
      bytes.concat(
        loadBytecode(
          string.concat(arcNetworkContractDir, "validator-manager/PermissionedValidatorManager.sol/PermissionedValidatorManager.json"),
          ".bytecode"
        ),
        abi.encode(address(validatorRegistryProxyAddr))
      )
    );
    vm.serializeString("output", "PermissionedValidatorManager", getJsonContractCode(address(poaManager)));

    // GasGuzzler
    address gasGuzzler = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "mocks/GasGuzzler.sol/GasGuzzler.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "GasGuzzler", getJsonContractCode(address(gasGuzzler)));

    // TestToken (ERC-20 for spammer load testing)
    address testToken = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "mocks/TestToken.sol/TestToken.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "TestToken", getJsonContractCode(address(testToken)));

    // Memo
    address memo = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "memo/Memo.sol/Memo.json"),
        ".bytecode"
      )
    );
    vm.serializeString("output", "Memo", getJsonContractCode(address(memo)));

    // Multicall3From
    address multicall3From = deployDeterministicContract(
      loadBytecode(
        string.concat(arcNetworkContractDir, "batch/Multicall3From.sol/Multicall3From.json"),
        ".bytecode"
      )
    );
    return vm.serializeString("output", "Multicall3From", getJsonContractCode(address(multicall3From)));
  }

  function run() public pure {
    console.log("usage: ArtifactHelper --sig 'run(uint256)' {chainId}");
  }

  function run(uint256 chainId, string memory outputPath, address validatorRegistryProxyAddr) public {
    vm.chainId(chainId);
    loadManifest("assets/artifacts/manifest.json");

    // External contracts
    for (uint256 i = 0; i < externalContracts.length; i++) {
      string memory contractName = externalContracts[i];
      DeploymentType deploymentType = getDeploymentType(contractName);
      if (deploymentType == DeploymentType.deterministic) {
        DeterministicDeployment memory deployment = loadDeterministicDeployment(contractName);
        address addr = deployDeterministicContract(deployment);
        vm.serializeString("output", contractName, getJsonContractCode(addr));
      } else {
        OneTimeAddressDeployment memory deployment = loadOneTimeAddressDeployment(contractName);
        address addr = deployOneTimeAddressContract(deployment);
        vm.serializeString("output", contractName, getJsonContractCode(addr));
      }
    }

    string memory stablecoinArtifactsDir = "assets/artifacts/stablecoin-contracts";

    // SignatureChecker
    address signatureCheckerAddr = deployDeterministicContract(
      loadBytecode(
        string.concat(stablecoinArtifactsDir, "/SignatureChecker.json"),
        ".bytecode")
    );
    vm.serializeString("output", "SignatureChecker", getJsonContractCode(signatureCheckerAddr));

    // NativeFiatTokenV2_2
    LinkReplacement[] memory linkReplacements = new LinkReplacement[](1);
    linkReplacements[0] = LinkReplacement({
      // This is a hardhat link reference placeholder for the SignatureChecker library.
      placeholder: "__$715109b5d747ea58b675c6ea3f0dba8c60$__",
      addr: signatureCheckerAddr
    });
    address fiatTokenAddr = deployDeterministicContract(
      loadBytecode(
        string.concat(stablecoinArtifactsDir, "/NativeFiatTokenV2_2.json"),
        ".bytecode",
        linkReplacements)
    );
    vm.serializeString("output", "NativeFiatTokenV2_2", getJsonContractCode(fiatTokenAddr));

    // FiatTokenProxy
    address fiatTokenProxyAddr = deployDeterministicContract(
      bytes.concat(
        loadBytecode(
          string.concat(stablecoinArtifactsDir, "/FiatTokenProxy.json"),
          ".bytecode"),
        abi.encode(fiatTokenAddr)
      )
    );
    vm.serializeString("output", "FiatTokenProxy", getJsonContractCode(fiatTokenProxyAddr));

    // Deploy ArcNetwork contracts (extracted to avoid stack too deep)
    string memory output = deployArcNetworkContracts("contracts/out/hardhat/contracts/src/", validatorRegistryProxyAddr);
    vm.writeJson(output, outputPath);
  }
}
