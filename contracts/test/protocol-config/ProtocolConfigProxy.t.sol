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
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";
import {ProtocolConfig} from "../../src/protocol-config/ProtocolConfig.sol";
import {IProtocolConfig} from "../../src/protocol-config/interfaces/IProtocolConfig.sol";

/**
 * @title ProtocolConfigProxyTest
 * @dev Test suite for ProtocolConfig when deployed behind AdminUpgradeableProxy
 * @dev Tests ProtocolConfig-specific functionality and business logic through proxy
 */
contract ProtocolConfigProxyTest is Test {
    // ============ Constants ============
    bytes32 constant PROTOCOL_CONFIG_STORAGE_LOCATION =
        0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200;

    // ============ State Variables ============

    AdminUpgradeableProxy public proxy;
    ProtocolConfig public implementation;
    ProtocolConfig public protocolConfig;
    address public actualProxyAdmin; // The actual admin address read from proxy storage

    // Test role addresses
    address public proxyAdminAddress; // The address we set as proxy admin
    address public controller; // Controller role for ProtocolConfig
    address public pauser; // Pauser role for ProtocolConfig
    address public implementationOwner; // Owner of the ProtocolConfig implementation
    address public rewardBeneficiary;

    // Default test parameters
    IProtocolConfig.FeeParams public defaultFeeParams = IProtocolConfig.FeeParams({
        alpha: 50,
        kRate: 75,
        inverseElasticityMultiplier: 5000,
        minBaseFee: 1000,
        maxBaseFee: 2000,
        blockGasLimit: 30000000
    });

    IProtocolConfig.ConsensusParams public defaultConsensusParams = IProtocolConfig.ConsensusParams({
        timeoutProposeMs: 2000,
        timeoutProposeDeltaMs: 200,
        timeoutPrevoteMs: 1000,
        timeoutPrevoteDeltaMs: 100,
        timeoutPrecommitMs: 1000,
        timeoutPrecommitDeltaMs: 100,
        timeoutRebroadcastMs: 2000,
        targetBlockTimeMs: 3000
    });

    // ============ Setup ============

    function setUp() public {
        // Create test addresses
        proxyAdminAddress = makeAddr("proxyAdminAddress");
        controller = makeAddr("controller");
        pauser = makeAddr("pauser");
        implementationOwner = makeAddr("implementationOwner");
        rewardBeneficiary = makeAddr("rewardBeneficiary");

        // Deploy implementation contract
        implementation = new ProtocolConfig();

        // Deploy proxy without initialization data (will be set via storage manipulation)
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyAdminAddress,
            "" // No initialization data - will be set through genesis-style storage manipulation
        );

        // Get proxy as ProtocolConfig interface
        protocolConfig = ProtocolConfig(address(proxy));

        // Simulate genesis file initialization by directly setting storage
        _simulateGenesisStorageInitialization(
            controller, pauser, defaultFeeParams, defaultConsensusParams, rewardBeneficiary
        );

        // Get the actual proxy admin address from ERC1967 storage
        actualProxyAdmin = address(
            uint160(
                uint256(vm.load(address(proxy), 0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103))
            )
        );
    }

    // Helper function to simulate genesis file initialization using storage manipulation
    function _simulateGenesisStorageInitialization(
        address _controller,
        address _pauser,
        IProtocolConfig.FeeParams memory _feeParams,
        IProtocolConfig.ConsensusParams memory _consensusParams,
        address _rewardBeneficiary
    ) internal {
        // === Set Controller storage ===
        // Controller.controller is in slot 0 (Ownable2StepUpgradeable uses ERC-7201 namespaced storage)
        // Controller: ERC-7201 slot for arc.storage.ProtocolConfigController
        bytes32 controllerSlot = 0x958f8fec699b51a1249f513eceda5429078000657f74abd1721bba363087af00;
        vm.store(address(protocolConfig), controllerSlot, bytes32(uint256(uint160(_controller))));

        // === Set Ownable owner in ERC-7201 storage ===
        // Calculate the correct ERC-7201 storage slot for Ownable owner
        // Pattern: keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff))
        bytes32 ownableStorageSlot =
            keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff));
        vm.store(address(protocolConfig), ownableStorageSlot, bytes32(uint256(uint160(_controller))));

        // === Set Pauser storage ===
        // Slot 1: pauser (address, offset 0) + paused (bool, offset 20) - packed together
        bytes32 packedPauserSlot = bytes32(
            uint256(uint160(_pauser)) // pauser in bytes 0-19
                | (uint256(0) << 160) // paused = false in byte 20 (offset 20)
        );
        // Pausable: ERC-7201 slot for arc.storage.Pausable
        // struct PausableStorage { address pauser; bool paused; }
        // Both packed into same slot: pauser (20 bytes) + paused (1 byte)
        bytes32 pausableSlot = 0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00;
        vm.store(address(protocolConfig), pausableSlot, packedPauserSlot);
        // === Set ProtocolConfig ERC-7201 storage ===
        // FeeParams struct - packed in storage slots
        // Slot 0: alpha (uint64) + kRate (uint64) + inverseElasticityMultiplier (uint64) + 8 bytes unused
        bytes32 packedSlot0 = bytes32(
            (uint256(_feeParams.alpha)) | (uint256(_feeParams.kRate) << 64)
                | (uint256(_feeParams.inverseElasticityMultiplier) << 128)
        );
        vm.store(address(protocolConfig), PROTOCOL_CONFIG_STORAGE_LOCATION, packedSlot0);

        // Slot 1: minBaseFee (uint256)
        vm.store(
            address(protocolConfig),
            bytes32(uint256(PROTOCOL_CONFIG_STORAGE_LOCATION) + 1),
            bytes32(uint256(_feeParams.minBaseFee))
        );

        // Slot 2: maxBaseFee (uint256)
        vm.store(
            address(protocolConfig),
            bytes32(uint256(PROTOCOL_CONFIG_STORAGE_LOCATION) + 2),
            bytes32(uint256(_feeParams.maxBaseFee))
        );

        // Slot 3: blockGasLimit (uint256)
        vm.store(
            address(protocolConfig),
            bytes32(uint256(PROTOCOL_CONFIG_STORAGE_LOCATION) + 3),
            bytes32(uint256(_feeParams.blockGasLimit))
        );

        // Slot 4: rewardBeneficiary (address)
        vm.store(
            address(protocolConfig),
            bytes32(uint256(PROTOCOL_CONFIG_STORAGE_LOCATION) + 4),
            bytes32(uint256(uint160(_rewardBeneficiary)))
        );

        // ConsensusParams struct layout:
        // - timeoutProposeMs (uint16), timeoutProposeDeltaMs (uint16), timeoutPrevoteMs (uint16),
        //   timeoutPrevoteDeltaMs (uint16), timeoutPrecommitMs (uint16), timeoutPrecommitDeltaMs (uint16),
        //   timeoutRebroadcastMs (uint16), targetBlockTimeMs (uint16)
        //   pack into one slot (8 * 16 = 128 bits, fits in one slot)

        // Slot 5: packed consensus params
        bytes32 packedConsensusSlot = bytes32(
            (uint256(_consensusParams.timeoutProposeMs)) | (uint256(_consensusParams.timeoutProposeDeltaMs) << 16)
                | (uint256(_consensusParams.timeoutPrevoteMs) << 32)
                | (uint256(_consensusParams.timeoutPrevoteDeltaMs) << 48)
                | (uint256(_consensusParams.timeoutPrecommitMs) << 64)
                | (uint256(_consensusParams.timeoutPrecommitDeltaMs) << 80)
                | (uint256(_consensusParams.timeoutRebroadcastMs) << 96)
                | (uint256(_consensusParams.targetBlockTimeMs) << 112)
        );
        vm.store(address(protocolConfig), bytes32(uint256(PROTOCOL_CONFIG_STORAGE_LOCATION) + 5), packedConsensusSlot);
    }

    // ============ ProtocolConfig Functionality Tests ============

    function test_ProtocolConfigInitialization() public view {
        // Verify ProtocolConfig is initialized correctly through proxy
        IProtocolConfig.FeeParams memory params = protocolConfig.feeParams();
        assertEq(params.alpha, defaultFeeParams.alpha);
        assertEq(params.kRate, defaultFeeParams.kRate);
        assertEq(params.inverseElasticityMultiplier, defaultFeeParams.inverseElasticityMultiplier);
        assertEq(params.minBaseFee, defaultFeeParams.minBaseFee);
        assertEq(params.maxBaseFee, defaultFeeParams.maxBaseFee);
        assertEq(params.blockGasLimit, defaultFeeParams.blockGasLimit);

        // Verify consensus params are initialized correctly
        IProtocolConfig.ConsensusParams memory consensusParams = protocolConfig.consensusParams();
        assertEq(consensusParams.timeoutProposeMs, defaultConsensusParams.timeoutProposeMs);
        assertEq(consensusParams.timeoutProposeDeltaMs, defaultConsensusParams.timeoutProposeDeltaMs);
        assertEq(consensusParams.timeoutPrevoteMs, defaultConsensusParams.timeoutPrevoteMs);
        assertEq(consensusParams.timeoutPrevoteDeltaMs, defaultConsensusParams.timeoutPrevoteDeltaMs);
        assertEq(consensusParams.timeoutPrecommitMs, defaultConsensusParams.timeoutPrecommitMs);
        assertEq(consensusParams.timeoutPrecommitDeltaMs, defaultConsensusParams.timeoutPrecommitDeltaMs);
        assertEq(consensusParams.timeoutRebroadcastMs, defaultConsensusParams.timeoutRebroadcastMs);
        assertEq(consensusParams.targetBlockTimeMs, defaultConsensusParams.targetBlockTimeMs);

        assertEq(protocolConfig.rewardBeneficiary(), rewardBeneficiary);
    }

    function test_UpdateFeeParamsViaProxy() public {
        // Test updating fee parameters through the proxy
        IProtocolConfig.FeeParams memory newParams = IProtocolConfig.FeeParams({
            alpha: 60,
            kRate: 80,
            inverseElasticityMultiplier: 3333,
            minBaseFee: 1500,
            maxBaseFee: 2500,
            blockGasLimit: 35000000
        });

        vm.prank(controller);
        protocolConfig.updateFeeParams(newParams);

        IProtocolConfig.FeeParams memory updatedParams = protocolConfig.feeParams();
        assertEq(updatedParams.alpha, 60);
        assertEq(updatedParams.kRate, 80);
        assertEq(updatedParams.inverseElasticityMultiplier, 3333);
        assertEq(updatedParams.minBaseFee, 1500);
        assertEq(updatedParams.maxBaseFee, 2500);
        assertEq(updatedParams.blockGasLimit, 35000000);
    }

    function test_UpdateConsensusParamsViaProxy() public {
        // Test updating consensus parameters through the proxy
        IProtocolConfig.ConsensusParams memory newParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 3000,
            timeoutProposeDeltaMs: 300,
            timeoutPrevoteMs: 2000,
            timeoutPrevoteDeltaMs: 200,
            timeoutPrecommitMs: 2000,
            timeoutPrecommitDeltaMs: 200,
            timeoutRebroadcastMs: 4000,
            targetBlockTimeMs: 6000
        });

        vm.prank(controller);
        protocolConfig.updateConsensusParams(newParams);

        IProtocolConfig.ConsensusParams memory updatedParams = protocolConfig.consensusParams();
        assertEq(updatedParams.timeoutProposeMs, 3000);
        assertEq(updatedParams.timeoutProposeDeltaMs, 300);
        assertEq(updatedParams.timeoutPrevoteMs, 2000);
        assertEq(updatedParams.timeoutPrevoteDeltaMs, 200);
        assertEq(updatedParams.timeoutPrecommitMs, 2000);
        assertEq(updatedParams.timeoutPrecommitDeltaMs, 200);
        assertEq(updatedParams.timeoutRebroadcastMs, 4000);
        assertEq(updatedParams.targetBlockTimeMs, 6000);
    }

    function test_UpdateRewardBeneficiaryViaProxy() public {
        address newBeneficiary = makeAddr("newBeneficiary");

        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);
    }

    function test_AccessControlViaProxy() public {
        // Test that access control works through the proxy
        IProtocolConfig.FeeParams memory newParams = IProtocolConfig.FeeParams({
            alpha: 70,
            kRate: 85,
            inverseElasticityMultiplier: 2500,
            minBaseFee: 2000,
            maxBaseFee: 3000,
            blockGasLimit: 40000000
        });

        // Non-controller should not be able to update
        vm.prank(implementationOwner);
        vm.expectRevert(); // Should revert with access control error
        protocolConfig.updateFeeParams(newParams);

        // Controller should be able to update
        vm.prank(controller);
        protocolConfig.updateFeeParams(newParams); // Should succeed

        IProtocolConfig.FeeParams memory updatedParams = protocolConfig.feeParams();
        assertEq(updatedParams.alpha, 70);
    }

    // ============ ProtocolConfig Upgrade Tests ============

    function test_ProtocolConfigUpgradeWithInitialization() public {
        // Deploy a new ProtocolConfig implementation
        ProtocolConfig newImplementation = new ProtocolConfig();

        // Now that owner is properly set, test updateController functionality
        // Controller is also the owner, so they can call updateController
        vm.prank(controller);
        protocolConfig.updateController(proxyAdminAddress);

        // Prepare initialization data for updateFeeParams (called by new controller = proxyAdminAddress)
        IProtocolConfig.FeeParams memory upgradeParams = IProtocolConfig.FeeParams({
            alpha: 90,
            kRate: 95,
            inverseElasticityMultiplier: 2000,
            minBaseFee: 3000,
            maxBaseFee: 4000,
            blockGasLimit: 45000000
        });

        bytes memory initData =
            abi.encodeWithSignature("updateFeeParams((uint64,uint64,uint64,uint256,uint256,uint256))", upgradeParams);

        // Perform upgrade with initialization
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall(address(newImplementation), initData);

        // Verify the upgrade succeeded and initialization was called
        IProtocolConfig.FeeParams memory updatedParams = protocolConfig.feeParams();
        assertEq(updatedParams.alpha, 90);
        assertEq(updatedParams.kRate, 95);
        assertEq(updatedParams.inverseElasticityMultiplier, 2000);
    }

    function test_ProtocolConfigStatePreservationAcrossUpgrade() public {
        // Modify state through proxy
        IProtocolConfig.FeeParams memory modifiedParams = IProtocolConfig.FeeParams({
            alpha: 77,
            kRate: 88,
            inverseElasticityMultiplier: 1666,
            minBaseFee: 1111,
            maxBaseFee: 2222,
            blockGasLimit: 33333333
        });

        vm.prank(controller);
        protocolConfig.updateFeeParams(modifiedParams);

        address newBeneficiary = makeAddr("upgradeBeneficiary");
        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Verify state before upgrade
        IProtocolConfig.FeeParams memory paramsBeforeUpgrade = protocolConfig.feeParams();
        assertEq(paramsBeforeUpgrade.alpha, 77);
        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);

        // Deploy new implementation and upgrade
        ProtocolConfig newImplementation = new ProtocolConfig();
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // Verify state is preserved after upgrade
        IProtocolConfig.FeeParams memory paramsAfterUpgrade = protocolConfig.feeParams();
        assertEq(paramsAfterUpgrade.alpha, 77);
        assertEq(paramsAfterUpgrade.kRate, 88);
        assertEq(paramsAfterUpgrade.inverseElasticityMultiplier, 1666);
        assertEq(paramsAfterUpgrade.minBaseFee, 1111);
        assertEq(paramsAfterUpgrade.maxBaseFee, 2222);
        assertEq(paramsAfterUpgrade.blockGasLimit, 33333333);
        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);
    }
}
