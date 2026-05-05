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
import {StdStorage, stdStorage} from "forge-std/StdStorage.sol";
import {ProtocolConfig} from "../../src/protocol-config/ProtocolConfig.sol";
import {IProtocolConfig} from "../../src/protocol-config/interfaces/IProtocolConfig.sol";
import {ProtocolConfigManagement} from "../../scripts/ProtocolConfigManagement.s.sol";

contract ProtocolConfigManagementTest is Test {
    using stdStorage for StdStorage;

    ProtocolConfig protocolConfig;
    ProtocolConfigManagement script;

    // Keys
    uint256 ownerPk = uint256(keccak256("OWNER_PK"));
    address owner = vm.addr(ownerPk);
    uint256 controllerPk = uint256(keccak256("CONTROLLER_PK"));
    address controller = vm.addr(controllerPk);

    address constant PROTOCOL_CONFIG_ADDR = 0x3600000000000000000000000000000000000001;

    function setUp() public {
        ProtocolConfig impl = new ProtocolConfig();
        script = new ProtocolConfigManagement();

        // Etch implementation code to the well-known address the script uses
        vm.etch(PROTOCOL_CONFIG_ADDR, address(impl).code);

        // Set controller at etched address (public variable)
        stdstore.target(PROTOCOL_CONFIG_ADDR).sig("controller()").checked_write(controller);

        // Bind handle to etched address for reads
        protocolConfig = ProtocolConfig(PROTOCOL_CONFIG_ADDR);

        // Seed initial fee params via controller so script preserves other fields
        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: 0,
            kRate: 0,
            minBaseFee: 1,
            maxBaseFee: 10,
            blockGasLimit: 30_000_000,
            inverseElasticityMultiplier: 5000
        });
        vm.prank(controller);
        protocolConfig.updateFeeParams(params);

        // Seed initial consensus params via controller
        IProtocolConfig.ConsensusParams memory consensusParams = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: 3000,
            timeoutProposeDeltaMs: 500,
            timeoutPrevoteMs: 1000,
            timeoutPrevoteDeltaMs: 500,
            timeoutPrecommitMs: 1000,
            timeoutPrecommitDeltaMs: 500,
            timeoutRebroadcastMs: 1000,
            targetBlockTimeMs: 500
        });
        vm.prank(controller);
        protocolConfig.updateConsensusParams(consensusParams);
    }

    function test_UpdateBlockGasLimit_Succeeds() public {
        vm.setEnv("CONTROLLER_KEY", vm.toString(controllerPk));

        script.updateBlockGasLimit(40_000_000);

        IProtocolConfig.FeeParams memory _feeParams = protocolConfig.feeParams();
        assertEq(_feeParams.blockGasLimit, 40_000_000);

        // unchanged initial parameters
        assertEq(_feeParams.minBaseFee, 1);
        assertEq(_feeParams.maxBaseFee, 10);
        assertEq(_feeParams.inverseElasticityMultiplier, 5000);
        assertEq(_feeParams.alpha, 0);
        assertEq(_feeParams.kRate, 0);
    }

    function test_UpdateTargetBlockTime_Succeeds() public {
        vm.setEnv("CONTROLLER_KEY", vm.toString(controllerPk));

        script.updateTargetBlockTime(250);

        IProtocolConfig.ConsensusParams memory _consensusParams = protocolConfig.consensusParams();
        assertEq(_consensusParams.timeoutProposeMs, 3000);
        assertEq(_consensusParams.timeoutProposeDeltaMs, 500);
        assertEq(_consensusParams.timeoutPrevoteMs, 1000);
        assertEq(_consensusParams.timeoutPrevoteDeltaMs, 500);
        assertEq(_consensusParams.timeoutPrecommitMs, 1000);
        assertEq(_consensusParams.timeoutPrecommitDeltaMs, 500);
        assertEq(_consensusParams.timeoutRebroadcastMs, 1000);
        assertEq(_consensusParams.targetBlockTimeMs, 250);
    }

    function test_UpdateBaseFeeBounds_Succeeds() public {
        vm.setEnv("CONTROLLER_KEY", vm.toString(controllerPk));

        // Update only the base fee bounds
        script.updateBaseFeeBounds(42, 1_000_000_000);

        IProtocolConfig.FeeParams memory _feeParams = protocolConfig.feeParams();
        assertEq(_feeParams.minBaseFee, 42);
        assertEq(_feeParams.maxBaseFee, 1_000_000_000);

        // unchanged initial parameters
        assertEq(_feeParams.blockGasLimit, 30_000_000);
        assertEq(_feeParams.inverseElasticityMultiplier, 5000);
        assertEq(_feeParams.alpha, 0);
        assertEq(_feeParams.kRate, 0);
    }
}
