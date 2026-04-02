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
import {console} from "forge-std/console.sol";
import {ProtocolConfig} from "../../src/protocol-config/ProtocolConfig.sol";
import {IProtocolConfig} from "../../src/protocol-config/interfaces/IProtocolConfig.sol";
import {Controller} from "../../src/protocol-config/roles/Controller.sol";
import {Pausable} from "../../src/common/roles/Pausable.sol";
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";

contract ProtocolConfigTest is Test {
    ProtocolConfig public protocolConfig;
    ProtocolConfig public implementation;
    AdminUpgradeableProxy public proxy;

    address public owner;
    address public controller;
    address public pauser;
    address public unauthorizedUser;
    address public proxyOwner;

    // Storage slot constants (matching the contract)
    bytes32 private constant PROTOCOL_CONFIG_STORAGE_LOCATION =
        0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200;

    // Events from contracts
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event ControllerUpdated(address indexed newController);
    event PauserChanged(address indexed newAddress);
    event FeeParamsUpdated(IProtocolConfig.FeeParams params);
    event ConsensusParamsUpdated(IProtocolConfig.ConsensusParams params);
    event RewardBeneficiaryUpdated(address indexed beneficiary);
    event Pause();
    event Unpause();

    function setUp() public {
        owner = makeAddr("owner");
        controller = makeAddr("controller");
        pauser = makeAddr("pauser");
        unauthorizedUser = makeAddr("unauthorizedUser");
        proxyOwner = makeAddr("proxyOwner");
    }

    // Helper function to deploy ProtocolConfig proxy with default values (simulating genesis initialization)
    function deployProtocolConfig(address _owner, address _controller, address _pauser)
        internal
        returns (ProtocolConfig)
    {
        // Default fee parameters for testing
        IProtocolConfig.FeeParams memory defaultFeeParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 75,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Default consensus parameters for testing
        IProtocolConfig.ConsensusParams memory defaultConsensusParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Default reward beneficiary
        address defaultRewardBeneficiary = makeAddr("defaultRewardBeneficiary");

        return deployProtocolConfig(
            _owner, _controller, _pauser, defaultFeeParams, defaultConsensusParams, defaultRewardBeneficiary
        );
    }

    // Helper function to deploy ProtocolConfig proxy with custom parameters (simulating genesis initialization)
    function deployProtocolConfig(
        address _owner,
        address _controller,
        address _pauser,
        IProtocolConfig.FeeParams memory _feeParams,
        IProtocolConfig.ConsensusParams memory _consensusParams,
        address _rewardBeneficiary
    ) internal returns (ProtocolConfig) {
        // Deploy implementation contract
        implementation = new ProtocolConfig();

        // Deploy proxy without initialization (empty data)
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyOwner,
            "" // No initialization data - will be set through genesis-style storage manipulation
        );

        // Get the proxy as ProtocolConfig interface
        ProtocolConfig deployedConfig = ProtocolConfig(address(proxy));

        // Simulate genesis file initialization by directly setting storage using calculated indices
        _simulateGenesisStorageInitialization(
            deployedConfig, _owner, _controller, _pauser, _feeParams, _consensusParams, _rewardBeneficiary
        );

        return deployedConfig;
    }

    // Helper function to simulate genesis file initialization using calculated storage indices
    function _simulateGenesisStorageInitialization(
        ProtocolConfig config,
        address _owner,
        address _controller,
        address _pauser,
        IProtocolConfig.FeeParams memory _feeParams,
        IProtocolConfig.ConsensusParams memory _consensusParams,
        address _rewardBeneficiary
    ) internal {
        // This simulates how genesis file would set storage slots directly

        // === Set Ownable storage (ERC-7201) ===
        // Owner is stored at ERC-7201 slot for "openzeppelin.storage.Ownable"
        bytes32 ownableSlot = 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300;
        vm.store(address(config), ownableSlot, bytes32(uint256(uint160(_owner))));

        // === Set Controller storage (ERC-7201) ===
        // Controller: arc.storage.ProtocolConfigController
        bytes32 controllerSlot = 0x958f8fec699b51a1249f513eceda5429078000657f74abd1721bba363087af00;
        vm.store(address(config), controllerSlot, bytes32(uint256(uint160(_controller))));

        // === Set Pausable storage (ERC-7201) ===
        // Pausable: arc.storage.Pausable
        // struct PausableStorage { address pauser; bool paused; }
        // Both packed into same slot: pauser (20 bytes) + paused (1 byte) = 21 bytes
        bytes32 pausableSlot = 0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00;
        bytes32 packedPausable = bytes32(
            uint256(uint160(_pauser)) // pauser in bytes 0-19
                | (uint256(0) << 160) // paused = false in byte 20
        );
        vm.store(address(config), pausableSlot, packedPausable);

        // === Set ERC-7201 ProtocolConfig storage ===
        // Base slot: 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200
        bytes32 baseSlot = PROTOCOL_CONFIG_STORAGE_LOCATION;

        // FeeParams struct layout:
        // - alpha (uint64), kRate (uint64), inverseElasticityMultiplier (uint64) pack into one slot
        // - minBaseFee (uint256) takes next slot
        // - maxBaseFee (uint256) takes next slot
        // - blockGasLimit (uint256) takes next slot
        // Then ConsensusParams struct takes next slots
        // Then rewardBeneficiary (address) takes next slot

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200: packed alpha|kRate|inverseElasticityMultiplier
        bytes32 packedSlot0 = bytes32(
            (uint256(_feeParams.alpha)) | (uint256(_feeParams.kRate) << 64)
                | (uint256(_feeParams.inverseElasticityMultiplier) << 128)
        );
        vm.store(address(config), baseSlot, packedSlot0);

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385201: minBaseFee
        vm.store(address(config), bytes32(uint256(baseSlot) + 1), bytes32(_feeParams.minBaseFee));

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385202: maxBaseFee
        vm.store(address(config), bytes32(uint256(baseSlot) + 2), bytes32(_feeParams.maxBaseFee));

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385203: blockGasLimit
        vm.store(address(config), bytes32(uint256(baseSlot) + 3), bytes32(_feeParams.blockGasLimit));

        // ConsensusParams struct layout:
        // - timeoutProposeMs (uint16), timeoutProposeDeltaMs (uint16), timeoutPrevoteMs (uint16),
        //   timeoutPrevoteDeltaMs (uint16), timeoutPrecommitMs (uint16), timeoutPrecommitDeltaMs (uint16),
        //   timeoutRebroadcastMs (uint16), targetBlockTimeMs (uint16)
        //   pack into one slot (8 * 16 = 128 bits, fits in one slot)

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385204: rewardBeneficiary
        vm.store(address(config), bytes32(uint256(baseSlot) + 4), bytes32(uint256(uint160(_rewardBeneficiary))));

        // Slot 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385205: packed consensus params
        bytes32 packedConsensusSlot = bytes32(
            (uint256(_consensusParams.timeoutProposeMs)) | (uint256(_consensusParams.timeoutProposeDeltaMs) << 16)
                | (uint256(_consensusParams.timeoutPrevoteMs) << 32)
                | (uint256(_consensusParams.timeoutPrevoteDeltaMs) << 48)
                | (uint256(_consensusParams.timeoutPrecommitMs) << 64)
                | (uint256(_consensusParams.timeoutPrecommitDeltaMs) << 80)
                | (uint256(_consensusParams.timeoutRebroadcastMs) << 96)
                | (uint256(_consensusParams.targetBlockTimeMs) << 112)
        );
        vm.store(address(config), bytes32(uint256(baseSlot) + 5), packedConsensusSlot);
    }

    // ============================================================================
    // GENESIS FILE HELPER FUNCTIONS
    // ============================================================================
    // Helper functions to generate genesis file allocation data

    /**
     * @notice Demonstrates genesis file storage allocation for ProtocolConfig
     * @dev Shows the exact key-value pairs needed in genesis alloc.storage field
     */
    function logGenesisStorageExample(
        address _owner,
        address _controller,
        address _pauser,
        IProtocolConfig.FeeParams memory _feeParams,
        IProtocolConfig.ConsensusParams memory _consensusParams,
        address _rewardBeneficiary
    ) public pure {
        // Log each storage slot individually to avoid stack too deep

        // Standard contract storage
        console.log("=== Genesis Storage Allocation ===");
        console.log("// Owner (ERC-7201 slot for openzeppelin.storage.Ownable)");
        console.log(
            '"0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300": "0x%s",', _toHexStringAddress(_owner)
        );

        console.log("// Controller (slot 0)");
        console.log(
            '"0x0000000000000000000000000000000000000000000000000000000000000000": "0x%s",',
            _toHexStringAddress(_controller)
        );

        console.log("// Pauser (slot 1)");
        console.log(
            '"0x0000000000000000000000000000000000000000000000000000000000000001": "0x%s",',
            _toHexStringAddress(_pauser)
        );

        console.log("// Paused = false (slot 2)");
        console.log(
            '"0x0000000000000000000000000000000000000000000000000000000000000002": "0x0000000000000000000000000000000000000000000000000000000000000000",'
        );

        // ERC-7201 storage
        console.log("// Packed fee params (base slot)");
        bytes32 packedValue = bytes32(
            (uint256(_feeParams.alpha)) | (uint256(_feeParams.kRate) << 64)
                | (uint256(_feeParams.inverseElasticityMultiplier) << 128)
        );
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200": "0x%s",',
            _toHexStringBytes32(packedValue)
        );

        console.log("// minBaseFee (base slot + 1)");
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385201": "0x%s",',
            _toHexStringBytes32(bytes32(_feeParams.minBaseFee))
        );

        console.log("// maxBaseFee (base slot + 2)");
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385202": "0x%s",',
            _toHexStringBytes32(bytes32(_feeParams.maxBaseFee))
        );

        console.log("// blockGasLimit (base slot + 3)");
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385203": "0x%s",',
            _toHexStringBytes32(bytes32(_feeParams.blockGasLimit))
        );

        console.log("// rewardBeneficiary (base slot + 4)");
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385204": "0x%s",',
            _toHexStringAddress(_rewardBeneficiary)
        );

        console.log("// consensus params (base slot + 5)");
        bytes32 packedConsensusValue = bytes32(
            (uint256(_consensusParams.timeoutProposeMs)) | (uint256(_consensusParams.timeoutProposeDeltaMs) << 16)
                | (uint256(_consensusParams.timeoutPrevoteMs) << 32)
                | (uint256(_consensusParams.timeoutPrevoteDeltaMs) << 48)
                | (uint256(_consensusParams.timeoutPrecommitMs) << 64)
                | (uint256(_consensusParams.timeoutPrecommitDeltaMs) << 80)
                | (uint256(_consensusParams.timeoutRebroadcastMs) << 96)
                | (uint256(_consensusParams.targetBlockTimeMs) << 112)
        );
        console.log(
            '"0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385205": "0x%s"',
            _toHexStringBytes32(packedConsensusValue)
        );
    }

    /**
     * @notice Test that demonstrates genesis file allocation generation
     */
    function test_generateGenesisAllocation() public {
        // Example values for genesis file
        address exampleOwner = makeAddr("exampleOwner");
        address exampleController = makeAddr("exampleController");
        address examplePauser = makeAddr("examplePauser");
        address exampleRewardBeneficiary = makeAddr("exampleRewardBeneficiary");

        IProtocolConfig.FeeParams memory exampleFeeParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 75,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000000000, // 1 gwei
            maxBaseFee: 500000000000, // 500 gwei
            blockGasLimit: 30000000
        });

        IProtocolConfig.ConsensusParams memory exampleConsensusParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Log the storage allocation for copy-paste into genesis file
        logGenesisStorageExample(
            exampleOwner,
            exampleController,
            examplePauser,
            exampleFeeParams,
            exampleConsensusParams,
            exampleRewardBeneficiary
        );
    }

    /**
     * @notice Convert address to hex string (64 chars, padded)
     */
    function _toHexStringAddress(address addr) internal pure returns (string memory) {
        return _toHexStringBytes32(bytes32(uint256(uint160(addr))));
    }

    /**
     * @notice Convert bytes32 to hex string (64 chars, no 0x prefix)
     */
    function _toHexStringBytes32(bytes32 value) internal pure returns (string memory) {
        bytes memory alphabet = "0123456789abcdef";
        bytes memory str = new bytes(64);
        for (uint256 i = 0; i < 32; i++) {
            str[i * 2] = alphabet[uint8(value[i] >> 4)];
            str[1 + i * 2] = alphabet[uint8(value[i] & 0x0f)];
        }
        return string(str);
    }

    // ============================================================================
    // DEPLOYMENT & STORAGE LAYOUT TESTS
    // ============================================================================
    // Tests for contract deployment and storage layout verification after genesis initialization

    function test_contractDeploymentAndGenesisInitialization() public {
        // Deploy with simulated genesis initialization
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Verify role assignments were set correctly through genesis storage manipulation
        assertEq(protocolConfig.owner(), owner);
        assertEq(protocolConfig.controller(), controller);
        assertEq(protocolConfig.pauser(), pauser);
        assertFalse(protocolConfig.paused());

        // Verify fee params are initialized to the default values from deployProtocolConfig
        IProtocolConfig.FeeParams memory params = protocolConfig.feeParams();
        assertEq(params.alpha, 50);
        assertEq(params.kRate, 75);
        assertEq(params.inverseElasticityMultiplier, 5000);
        assertEq(params.minBaseFee, 1000);
        assertEq(params.maxBaseFee, 2000);
        assertEq(params.blockGasLimit, 30000000);

        // Verify consensus params are initialized to the default values
        IProtocolConfig.ConsensusParams memory consensusParams = protocolConfig.consensusParams();
        assertEq(consensusParams.timeoutProposeMs, 2000);
        assertEq(consensusParams.timeoutProposeDeltaMs, 200);
        assertEq(consensusParams.timeoutPrevoteMs, 1000);
        assertEq(consensusParams.timeoutPrevoteDeltaMs, 100);
        assertEq(consensusParams.timeoutPrecommitMs, 1000);
        assertEq(consensusParams.timeoutPrecommitDeltaMs, 100);
        assertEq(consensusParams.timeoutRebroadcastMs, 2000);
        assertEq(consensusParams.targetBlockTimeMs, 3000);

        // Verify reward beneficiary is set to the default value
        assertEq(protocolConfig.rewardBeneficiary(), makeAddr("defaultRewardBeneficiary"));
    }

    function test_implementationContractState() public {
        // Test the actual implementation contract before genesis initialization
        ProtocolConfig impl = new ProtocolConfig();

        // Implementation should be uninitialized (upgradeable pattern)
        assertEq(impl.owner(), address(0)); // No initialization on implementation
        assertEq(impl.controller(), address(0)); // Not set
        assertEq(impl.pauser(), address(0)); // Not set
        assertFalse(impl.paused()); // Initial state

        // Storage should be empty (all zeros)
        IProtocolConfig.FeeParams memory params = impl.feeParams();
        assertEq(params.alpha, 0);
        assertEq(params.kRate, 0);
        assertEq(params.inverseElasticityMultiplier, 0);
        assertEq(params.minBaseFee, 0);
        assertEq(params.maxBaseFee, 0);
        assertEq(params.blockGasLimit, 0);
        assertEq(impl.rewardBeneficiary(), address(0));
    }

    function test_storageLayoutCalculation() public {
        // Test that our storage layout calculations are correct by setting and reading specific values
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Test extreme values to ensure correct slot packing/unpacking
        IProtocolConfig.FeeParams memory extremeParams = IProtocolConfig.FeeParams({
            alpha: 100, // Max uint64 we allow
            kRate: 99, // Near max
            inverseElasticityMultiplier: type(uint64).max, // Actual max uint64
            minBaseFee: 1, // Min value
            maxBaseFee: type(uint256).max, // Max uint256
            blockGasLimit: type(uint256).max // Max uint256
        });

        address extremeBeneficiary = address(type(uint160).max); // Max address

        // Deploy with extreme values
        IProtocolConfig.ConsensusParams memory extremeConsensusParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });
        ProtocolConfig extremeConfig =
            deployProtocolConfig(owner, controller, pauser, extremeParams, extremeConsensusParams, extremeBeneficiary);

        // Verify extreme values are stored and retrieved correctly
        IProtocolConfig.FeeParams memory retrievedParams = extremeConfig.feeParams();
        assertEq(retrievedParams.alpha, 100);
        assertEq(retrievedParams.kRate, 99);
        assertEq(retrievedParams.inverseElasticityMultiplier, type(uint64).max);
        assertEq(retrievedParams.minBaseFee, 1);
        assertEq(retrievedParams.maxBaseFee, type(uint256).max);
        assertEq(retrievedParams.blockGasLimit, type(uint256).max);
        assertEq(extremeConfig.rewardBeneficiary(), extremeBeneficiary);
    }

    function test_directStorageAccess() public {
        // Test that we can read the storage values directly to verify our layout calculations
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Read the ERC-7201 storage directly
        bytes32 baseSlot = PROTOCOL_CONFIG_STORAGE_LOCATION;

        // Read packed slot 0 (alpha, kRate, inverseElasticityMultiplier)
        bytes32 packedSlot0 = vm.load(address(protocolConfig), baseSlot);
        uint64 storedAlpha = uint64(uint256(packedSlot0));
        uint64 storedKRate = uint64(uint256(packedSlot0) >> 64);
        uint64 storedInverseElasticityMultiplier = uint64(uint256(packedSlot0) >> 128);

        assertEq(storedAlpha, 50);
        assertEq(storedKRate, 75);
        assertEq(storedInverseElasticityMultiplier, 5000);

        // Read other slots
        bytes32 minBaseFeeSlot = vm.load(address(protocolConfig), bytes32(uint256(baseSlot) + 1));
        bytes32 maxBaseFeeSlot = vm.load(address(protocolConfig), bytes32(uint256(baseSlot) + 2));
        bytes32 blockGasLimitSlot = vm.load(address(protocolConfig), bytes32(uint256(baseSlot) + 3));
        bytes32 rewardBeneficiarySlot = vm.load(address(protocolConfig), bytes32(uint256(baseSlot) + 4));
        bytes32 consensusParamsSlot = vm.load(address(protocolConfig), bytes32(uint256(baseSlot) + 5));

        assertEq(uint256(minBaseFeeSlot), 1000);
        assertEq(uint256(maxBaseFeeSlot), 2000);
        assertEq(uint256(blockGasLimitSlot), 30000000);
        assertEq(address(uint160(uint256(rewardBeneficiarySlot))), makeAddr("defaultRewardBeneficiary"));

        // Verify consensus params are packed correctly
        uint16 storedTimeoutProposeMs = uint16(uint256(consensusParamsSlot));
        uint16 storedTimeoutProposeDeltaMs = uint16(uint256(consensusParamsSlot) >> 16);
        uint16 storedTimeoutPrevoteMs = uint16(uint256(consensusParamsSlot) >> 32);
        uint16 storedTimeoutPrevoteDeltaMs = uint16(uint256(consensusParamsSlot) >> 48);
        uint16 storedTimeoutPrecommitMs = uint16(uint256(consensusParamsSlot) >> 64);
        uint16 storedTimeoutPrecommitDeltaMs = uint16(uint256(consensusParamsSlot) >> 80);
        uint16 storedTimeoutRebroadcastMs = uint16(uint256(consensusParamsSlot) >> 96);
        uint16 storedTargetBlockTimeMs = uint16(uint256(consensusParamsSlot) >> 112);

        assertEq(storedTimeoutProposeMs, 2000);
        assertEq(storedTimeoutProposeDeltaMs, 200);
        assertEq(storedTimeoutPrevoteMs, 1000);
        assertEq(storedTimeoutPrevoteDeltaMs, 100);
        assertEq(storedTimeoutPrecommitMs, 1000);
        assertEq(storedTimeoutPrecommitDeltaMs, 100);
        assertEq(storedTimeoutRebroadcastMs, 2000);
        assertEq(storedTargetBlockTimeMs, 3000);
    }

    // ============================================================================
    // FEE PARAMETERS VALIDATION TESTS
    // ============================================================================
    // Tests for updateFeeParams validation logic and boundary conditions

    function test_updateFeeParams__ValidParams() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory newParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 75,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should succeed when called by controller
        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit FeeParamsUpdated(newParams);
        protocolConfig.updateFeeParams(newParams);

        // Verify the params were updated
        IProtocolConfig.FeeParams memory updatedParams = protocolConfig.feeParams();
        assertEq(updatedParams.alpha, 50);
        assertEq(updatedParams.kRate, 75);
        assertEq(updatedParams.inverseElasticityMultiplier, 5000);
        assertEq(updatedParams.minBaseFee, 1000);
        assertEq(updatedParams.maxBaseFee, 2000);
        assertEq(updatedParams.blockGasLimit, 30000000);
    }

    function test_updateFeeParams__InvalidAlpha() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Test validation: alpha value must be <= 100
        IProtocolConfig.FeeParams memory invalidParams = IProtocolConfig.FeeParams({
            alpha: 101, // Invalid: exceeds maximum allowed value of 100
            kRate: 50,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should get specific validation error (not generic access control error)
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidAlpha.selector);
        protocolConfig.updateFeeParams(invalidParams);
    }

    function test_updateFeeParams__InvalidKRate() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory invalidParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 10001, // Invalid: > 10000
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidKRate.selector);
        protocolConfig.updateFeeParams(invalidParams);
    }

    function test_updateFeeParams__InvalidBaseFee() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory invalidParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 50,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 2000, // Invalid: > maxBaseFee
            maxBaseFee: 1000,
            blockGasLimit: 30000000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidBaseFeeRange.selector);
        protocolConfig.updateFeeParams(invalidParams);
    }

    function test_updateFeeParams__InvalidBlockGasLimit() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory invalidParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 50,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 0 // Invalid: must be > 0
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidBlockGasLimit.selector);
        protocolConfig.updateFeeParams(invalidParams);
    }

    function test_updateFeeParams__InvalidElasticityMultiplier() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory invalidParams = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 50,
            inverseElasticityMultiplier: 10001, // Invalid: must be <= 10000
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidInverseElasticityMultiplier.selector);
        protocolConfig.updateFeeParams(invalidParams);
    }

    function test_updateFeeParams__UnauthorizedUser() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 50,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should revert when called by unauthorized user
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateFeeParams(params);
    }

    function test_updateBlockGasLimit__Succeeds() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory beforeParams = protocolConfig.feeParams();
        uint256 newBlockGasLimit = beforeParams.blockGasLimit + 1;

        IProtocolConfig.FeeParams memory expected = beforeParams;
        expected.blockGasLimit = newBlockGasLimit;

        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit FeeParamsUpdated(expected);
        protocolConfig.updateBlockGasLimit(newBlockGasLimit);

        IProtocolConfig.FeeParams memory afterParams = protocolConfig.feeParams();
        assertEq(afterParams.blockGasLimit, newBlockGasLimit);
        assertEq(afterParams.alpha, beforeParams.alpha);
        assertEq(afterParams.kRate, beforeParams.kRate);
        assertEq(afterParams.inverseElasticityMultiplier, beforeParams.inverseElasticityMultiplier);
        assertEq(afterParams.minBaseFee, beforeParams.minBaseFee);
        assertEq(afterParams.maxBaseFee, beforeParams.maxBaseFee);
    }

    function test_updateBlockGasLimit__RevertsZero() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidBlockGasLimit.selector);
        protocolConfig.updateBlockGasLimit(0);
    }

    function test_updateBlockGasLimit__RevertsNotAuthorized() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateBlockGasLimit(40_000_000);
    }

    function test_updateBlockGasLimit__RevertsWhenPaused() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        vm.prank(pauser);
        protocolConfig.pause();

        vm.prank(controller);
        vm.expectRevert(Pausable.ContractPaused.selector);
        protocolConfig.updateBlockGasLimit(40_000_000);
    }

    // ============================================================================
    // CONSENSUS PARAMETERS VALIDATION TESTS
    // ============================================================================
    // Tests for updateConsensusParams validation logic and boundary conditions

    function test_updateConsensusParams__ValidParams() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

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

        // Should succeed when called by controller
        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit ConsensusParamsUpdated(newParams);
        protocolConfig.updateConsensusParams(newParams);

        // Verify the params were updated
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

    function test_updateConsensusParams__InvalidTimeoutProposeMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 0, // Invalid: must be > 0
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutProposeMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutProposeDeltaMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 0, // Invalid: must be > 0
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutProposeDeltaMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutPrevoteMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 0, // Invalid: must be > 0
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutPrevoteMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutPrevoteDeltaMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 0, // Invalid: must be > 0
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutPrevoteDeltaMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutPrecommitMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 0, // Invalid: must be > 0
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutPrecommitMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutPrecommitDeltaMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 0, // Invalid: must be > 0
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutPrecommitDeltaMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__InvalidTimeoutRebroadcastMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory invalidParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 0, // Invalid: must be > 0
            targetBlockTimeMs: 3000
        });

        // Should revert with specific validation error
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutRebroadcastMs.selector);
        protocolConfig.updateConsensusParams(invalidParams);
    }

    function test_updateConsensusParams__ZeroTargetBlockTimeMs() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory paramsWithZero = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 2000,
            timeoutProposeDeltaMs: 200,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 100,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 100,
            timeoutRebroadcastMs: 2000,
            targetBlockTimeMs: 0 // 0 means disabled
        });

        vm.prank(controller);
        protocolConfig.updateConsensusParams(paramsWithZero);
    }

    function test_updateConsensusParams__UnauthorizedUser() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory params = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 3000,
            timeoutProposeDeltaMs: 300,
            timeoutPrevoteMs: 2000,
            timeoutPrevoteDeltaMs: 200,
            timeoutPrecommitMs: 2000,
            timeoutPrecommitDeltaMs: 200,
            timeoutRebroadcastMs: 4000,
            targetBlockTimeMs: 6000
        });

        // Should revert when called by unauthorized user
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateConsensusParams(params);
    }

    function test_updateTimeoutProposeMs__Valid() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.ConsensusParams memory beforeParams = protocolConfig.consensusParams();
        uint16 newTimeout = beforeParams.timeoutProposeMs + 1;

        IProtocolConfig.ConsensusParams memory expected = beforeParams;
        expected.timeoutProposeMs = newTimeout;

        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit ConsensusParamsUpdated(expected);
        protocolConfig.updateTimeoutProposeMs(newTimeout);

        IProtocolConfig.ConsensusParams memory afterParams = protocolConfig.consensusParams();
        assertEq(afterParams.timeoutProposeMs, newTimeout);
        assertEq(afterParams.timeoutProposeDeltaMs, beforeParams.timeoutProposeDeltaMs);
        assertEq(afterParams.timeoutPrevoteMs, beforeParams.timeoutPrevoteMs);
        assertEq(afterParams.timeoutPrevoteDeltaMs, beforeParams.timeoutPrevoteDeltaMs);
        assertEq(afterParams.timeoutPrecommitMs, beforeParams.timeoutPrecommitMs);
        assertEq(afterParams.timeoutPrecommitDeltaMs, beforeParams.timeoutPrecommitDeltaMs);
        assertEq(afterParams.timeoutRebroadcastMs, beforeParams.timeoutRebroadcastMs);
        assertEq(afterParams.targetBlockTimeMs, beforeParams.targetBlockTimeMs);
    }

    function test_updateTimeoutProposeMs__RevertsZero() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);
        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidTimeoutProposeMs.selector);
        protocolConfig.updateTimeoutProposeMs(0);
    }

    function test_updateTimeoutProposeMs__Unauthorized() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateTimeoutProposeMs(123);
    }

    function test_updateTimeoutProposeMs__Paused() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        vm.prank(pauser);
        protocolConfig.pause();

        vm.prank(controller);
        vm.expectRevert(Pausable.ContractPaused.selector);
        protocolConfig.updateTimeoutProposeMs(123);
    }

    // ============================================================================
    // REWARD BENEFICIARY TESTS
    // ============================================================================
    // Tests for updateRewardBeneficiary functionality and validation

    function test_updateRewardBeneficiary__ValidAddress() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        address newBeneficiary = makeAddr("newBeneficiary");

        // Should succeed when called by controller
        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit RewardBeneficiaryUpdated(newBeneficiary);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Verify the beneficiary was updated
        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);
    }

    function test_updateRewardBeneficiary__ZeroAddress() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Should allow setting beneficiary to zero address
        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(address(0));

        // Verify the beneficiary was updated
        assertEq(protocolConfig.rewardBeneficiary(), address(0));
    }

    // ============================================================================
    // PAUSE FUNCTIONALITY TESTS
    // ============================================================================
    // Tests for pause/unpause behavior and whenNotPaused modifier

    function test_pauseAndUnpause() public {
        // Deploy contract
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Should allow pauser to pause
        vm.prank(pauser);
        vm.expectEmit(false, false, false, false);
        emit Pause();
        protocolConfig.pause();

        assertTrue(protocolConfig.paused());

        // Operations should be blocked when paused
        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 50,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        vm.prank(controller);
        vm.expectRevert(Pausable.ContractPaused.selector);
        protocolConfig.updateFeeParams(params);

        // Should allow pauser to unpause
        vm.prank(pauser);
        vm.expectEmit(false, false, false, false);
        emit Unpause();
        protocolConfig.unpause();

        assertFalse(protocolConfig.paused());

        // Operations should work again after unpause
        vm.prank(controller);
        protocolConfig.updateFeeParams(params);
    }

    // ============================================================================
    // ACCESS CONTROL TESTS
    // ============================================================================
    // Tests for role-based access control (owner, controller, pauser)

    function test_accessControl_UnauthorizedUser() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 75,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Unauthorized user should still get controller error
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateFeeParams(params);

        // Owner should also get controller error (only controller can call)
        vm.prank(owner);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateFeeParams(params);
    }

    // ============================================================================
    // VIEW FUNCTIONS & STATE TESTS
    // ============================================================================
    // Tests for reading contract state and view function behavior

    function test_viewFunctions() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Test that view functions work and return the initialized default values
        IProtocolConfig.FeeParams memory params = protocolConfig.feeParams();
        assertEq(params.alpha, 50);
        assertEq(params.kRate, 75);
        assertEq(params.inverseElasticityMultiplier, 5000);
        assertEq(params.minBaseFee, 1000);
        assertEq(params.maxBaseFee, 2000);
        assertEq(params.blockGasLimit, 30000000);

        assertEq(protocolConfig.rewardBeneficiary(), makeAddr("defaultRewardBeneficiary"));

        // Also verify initialized state
        assertEq(protocolConfig.owner(), owner);
        assertEq(protocolConfig.controller(), controller);
        assertEq(protocolConfig.pauser(), pauser);
        assertFalse(protocolConfig.paused());
    }

    function test_pauserAccessControl() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Unauthorized user should not be able to pause
        vm.prank(unauthorizedUser);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();

        // Controller should not be able to pause (only pauser can)
        vm.prank(controller);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();

        // Owner should not be able to pause (only pauser can)
        vm.prank(owner);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();

        // Pauser should be able to pause
        vm.prank(pauser);
        protocolConfig.pause();
        assertTrue(protocolConfig.paused());

        // Pauser should be able to unpause
        vm.prank(pauser);
        protocolConfig.unpause();
        assertFalse(protocolConfig.paused());
    }

    function test_ownerAccessControl() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        address newController = makeAddr("newController");
        address newPauser = makeAddr("newPauser");

        // Owner should be able to update controller
        vm.prank(owner);
        vm.expectEmit(true, true, true, true);
        emit ControllerUpdated(newController);
        protocolConfig.updateController(newController);
        assertEq(protocolConfig.controller(), newController);

        // Owner should be able to update pauser
        vm.prank(owner);
        vm.expectEmit(true, true, true, true);
        emit PauserChanged(newPauser);
        protocolConfig.updatePauser(newPauser);
        assertEq(protocolConfig.pauser(), newPauser);

        // Unauthorized user should not be able to update controller
        vm.prank(unauthorizedUser);
        vm.expectRevert();
        protocolConfig.updateController(controller);

        // Controller should not be able to update pauser
        vm.prank(newController);
        vm.expectRevert();
        protocolConfig.updatePauser(pauser);
    }

    function test_updateController_ZeroAddressValidation() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Should revert when trying to set controller to zero address
        vm.prank(owner);
        vm.expectRevert(Controller.ZeroControllerAddress.selector);
        protocolConfig.updateController(address(0));
    }

    function test_updatePauser_ZeroAddressValidation() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Should revert when trying to set pauser to zero address
        vm.prank(owner);
        vm.expectRevert(Pausable.ZeroPauserAddress.selector);
        protocolConfig.updatePauser(address(0));
    }

    function test_contractStateTransitions() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Verify properly initialized state
        assertEq(protocolConfig.owner(), owner);
        assertEq(protocolConfig.controller(), controller);
        assertEq(protocolConfig.pauser(), pauser);
        assertFalse(protocolConfig.paused());
        assertEq(protocolConfig.rewardBeneficiary(), makeAddr("defaultRewardBeneficiary")); // Set to default value

        // Verify fee params are set to default values
        IProtocolConfig.FeeParams memory params = protocolConfig.feeParams();
        assertEq(params.alpha, 50);
        assertEq(params.blockGasLimit, 30000000);

        // Test state transitions work correctly
        address newBeneficiary = makeAddr("beneficiary");
        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);
        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);

        // Test pause state transition
        vm.prank(pauser);
        protocolConfig.pause();
        assertTrue(protocolConfig.paused());

        vm.prank(pauser);
        protocolConfig.unpause();
        assertFalse(protocolConfig.paused());
    }

    function test_extremeFeeParamsValues() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Test with extreme but valid values - should succeed
        IProtocolConfig.FeeParams memory extremeParams = IProtocolConfig.FeeParams({
            alpha: 100, // Maximum valid value
            kRate: 10000, // Maximum valid value
            inverseElasticityMultiplier: 10000,
            minBaseFee: 1,
            maxBaseFee: type(uint256).max,
            blockGasLimit: type(uint256).max
        });

        vm.prank(controller);
        protocolConfig.updateFeeParams(extremeParams);

        // Verify the extreme values were set
        IProtocolConfig.FeeParams memory setParams = protocolConfig.feeParams();
        assertEq(setParams.alpha, 100);
        assertEq(setParams.kRate, 10000);
        assertEq(setParams.inverseElasticityMultiplier, 10000);

        // Test edge case where minBaseFee equals maxBaseFee - should be valid
        extremeParams.minBaseFee = 1000;
        extremeParams.maxBaseFee = 1000;

        vm.prank(controller);
        protocolConfig.updateFeeParams(extremeParams);

        // Test with invalid extreme values (alpha > 100)
        IProtocolConfig.FeeParams memory invalidExtremeParams = IProtocolConfig.FeeParams({
            alpha: type(uint64).max, // Invalid: > 100
            kRate: 50,
            inverseElasticityMultiplier: 10000,
            minBaseFee: 1,
            maxBaseFee: type(uint256).max,
            blockGasLimit: type(uint256).max
        });

        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidAlpha.selector);
        protocolConfig.updateFeeParams(invalidExtremeParams);

        // Test with invalid kRate > 10000
        invalidExtremeParams.alpha = 50;
        invalidExtremeParams.kRate = type(uint64).max; // Invalid: > 10000

        vm.prank(controller);
        vm.expectRevert(ProtocolConfig.InvalidKRate.selector);
        protocolConfig.updateFeeParams(invalidExtremeParams);
    }

    function test_updateRewardBeneficiary_OnlyController() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        address newBeneficiary = makeAddr("newBeneficiary");

        // Controller should be able to update beneficiary
        vm.prank(controller);
        vm.expectEmit(true, true, true, true);
        emit RewardBeneficiaryUpdated(newBeneficiary);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);
        assertEq(protocolConfig.rewardBeneficiary(), newBeneficiary);

        // Should fail - unauthorized user
        vm.prank(unauthorizedUser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Should fail - owner is not controller
        vm.prank(owner);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Should fail - pauser is not controller
        vm.prank(pauser);
        vm.expectRevert(Controller.CallerIsNotController.selector);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);
    }

    function test_updateRewardBeneficiary_ZeroAddressValidation() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Validation logic should work properly
        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(address(0));
        assertEq(protocolConfig.rewardBeneficiary(), address(0));
    }

    function test_pauseBehaviorAccess() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Pauser should be able to pause
        vm.prank(pauser);
        protocolConfig.pause();
        assertTrue(protocolConfig.paused());

        // Pauser should be able to unpause
        vm.prank(pauser);
        protocolConfig.unpause();
        assertFalse(protocolConfig.paused());

        // Unauthorized user cannot pause
        vm.prank(unauthorizedUser);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();

        // Owner cannot pause (only pauser can)
        vm.prank(owner);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();

        // Controller cannot pause (only pauser can)
        vm.prank(controller);
        vm.expectRevert(Pausable.CallerIsNotPauser.selector);
        protocolConfig.pause();
    }

    function test_whenNotPausedModifier() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // The whenNotPaused modifier should work correctly
        // Since the contract starts unpaused, operations should be allowed

        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: 50,
            kRate: 75,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        // Should succeed when unpaused
        vm.prank(controller);
        protocolConfig.updateFeeParams(params);

        address newBeneficiary = makeAddr("newBeneficiary");
        vm.prank(controller);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Now pause the contract
        vm.prank(pauser);
        protocolConfig.pause();
        assertTrue(protocolConfig.paused());

        // Operations should fail when paused
        vm.prank(controller);
        vm.expectRevert(Pausable.ContractPaused.selector);
        protocolConfig.updateFeeParams(params);

        vm.prank(controller);
        vm.expectRevert(Pausable.ContractPaused.selector);
        protocolConfig.updateRewardBeneficiary(newBeneficiary);

        // Unpause and operations should work again
        vm.prank(pauser);
        protocolConfig.unpause();
        assertFalse(protocolConfig.paused());

        vm.prank(controller);
        protocolConfig.updateFeeParams(params);
    }

    // ============ Alpha and kRate Validation Tests ============

    function test_alphaKRateValidation_BoundaryValues() public {
        protocolConfig = deployProtocolConfig(owner, controller, pauser);

        // Test boundary values for alpha (0-100) and kRate (0-10000) should be valid
        IProtocolConfig.FeeParams memory validParams;

        // Test alpha = 0, kRate = 0 (should pass validation and succeed)
        validParams = IProtocolConfig.FeeParams({
            alpha: 0,
            kRate: 0,
            inverseElasticityMultiplier: 5000,
            minBaseFee: 1000,
            maxBaseFee: 2000,
            blockGasLimit: 30000000
        });

        vm.prank(controller);
        protocolConfig.updateFeeParams(validParams);

        // Verify the boundary values were set
        IProtocolConfig.FeeParams memory setParams = protocolConfig.feeParams();
        assertEq(setParams.alpha, 0);
        assertEq(setParams.kRate, 0);

        // Test alpha = 100, kRate = 10000 (should pass validation and succeed)
        validParams.alpha = 100;
        validParams.kRate = 10000;

        vm.prank(controller);
        protocolConfig.updateFeeParams(validParams);

        // Verify the maximum boundary values were set
        IProtocolConfig.FeeParams memory maxParams = protocolConfig.feeParams();
        assertEq(maxParams.alpha, 100);
        assertEq(maxParams.kRate, 10000);
    }

    // ============================================================================
    // STORAGE & ERC-7201 TESTS
    // ============================================================================
    // Tests for storage layout and ERC-7201 namespaced storage implementation

    function test_erc7201StorageSlotCalculation() public pure {
        // Test the ERC-7201 storage slot calculation is correct
        // Formula: keccak256(abi.encode(uint256(keccak256("namespace")) - 1)) & ~bytes32(uint256(0xff))

        string memory namespace = "arc.storage.ProtocolConfig";
        bytes32 expectedSlot = keccak256(abi.encode(uint256(keccak256(bytes(namespace))) - 1)) & ~bytes32(uint256(0xff));

        // The expected slot should match the constant in the contract
        bytes32 contractSlot = 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200;

        assertEq(expectedSlot, contractSlot, "ERC-7201 storage slot calculation mismatch");
    }

    function test_erc7201StorageLayout() public {
        // Test the storage layout within our custom storage slot
        ProtocolConfig newContract = new ProtocolConfig();

        // The storage struct should have:
        // - FeeParams feeParams (first field, multiple slots)
        // - address rewardBeneficiary (after FeeParams)

        // Test initial values are zero (default)
        IProtocolConfig.FeeParams memory params = newContract.feeParams();
        assertEq(params.alpha, 0);
        assertEq(params.kRate, 0);
        assertEq(params.inverseElasticityMultiplier, 0);
        assertEq(params.minBaseFee, 0);
        assertEq(params.maxBaseFee, 0);
        assertEq(params.blockGasLimit, 0);
        assertEq(newContract.rewardBeneficiary(), address(0));
    }
}
