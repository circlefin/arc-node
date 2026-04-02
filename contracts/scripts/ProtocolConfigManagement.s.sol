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
import {IProtocolConfig} from "../src/protocol-config/interfaces/IProtocolConfig.sol";
import {Addresses} from "./Addresses.sol";

/**
 * @notice Helper script for managing ProtocolConfig parameters
 * @dev Usage:
 *
 * ============ Print Functions ============
 * Print fee params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "printFeeParams()"
 *
 * Print consensus params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "printConsensusParams()"
 *
 * Print reward beneficiary:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "printRewardBeneficiary()"
 *
 * Print all params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "printAllParams()"
 *
 * ============ Update Fee Params (controller only, requires CONTROLLER_KEY env var) ============
 * Update block gas limit:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateBlockGasLimit(uint64)" <newLimit> --broadcast
 *
 * Update base fee bounds:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateBaseFeeBounds(uint256,uint256)" <newMin> <newMax> --broadcast
 *
 * Update alpha:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateAlpha(uint64)" <newAlpha> --broadcast
 *
 * Update kRate:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateKRate(uint64)" <newKRate> --broadcast
 *
 * Update elasticity multiplier:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateElasticityMultiplier(uint64)" <newMultiplier> --broadcast
 *
 * Update all fee params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateAllFeeParams(uint64,uint64,uint64,uint256,uint256,uint256)" <alpha> <kRate> <elasticity> <minBaseFee> <maxBaseFee> <blockGasLimit> --broadcast
 *
 * ============ Update Consensus Params (controller only, requires CONTROLLER_KEY env var) ============
 * Update target block time:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTargetBlockTime(uint16)" <newTimeMs> --broadcast
 *
 * Update individual timeout params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutPropose(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutProposeDelta(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutPrevote(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutPrevoteDelta(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutPrecommit(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutPrecommitDelta(uint16)" <newTimeMs> --broadcast
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateTimeoutRebroadcast(uint16)" <newTimeMs> --broadcast
 *
 * Update all consensus params:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateAllConsensusParams(uint16,uint16,uint16,uint16,uint16,uint16,uint16,uint16)" <propose> <proposeDelta> <prevote> <prevoteDelta> <precommit> <precommitDelta> <rebroadcast> <targetBlockTime> --broadcast
 *
 * ============ Update Reward Beneficiary (controller only, requires CONTROLLER_KEY env var) ============
 * Update reward beneficiary:
 *   forge script scripts/ProtocolConfigManagement.s.sol --rpc-url <network> --sig "updateRewardBeneficiary(address)" <newBeneficiary> --broadcast
 */
contract ProtocolConfigManagement is Script {
    // ============ Constants ============

    ProtocolConfig PROTOCOL_CONFIG = ProtocolConfig(Addresses.PROTOCOL_CONFIG);

    // ============ Helpers ============

    function printFeeParams() public view returns (IProtocolConfig.FeeParams memory params) {
        params = PROTOCOL_CONFIG.feeParams();

        console.log("ProtocolConfig FeeParams:");
        console.log("alpha:", params.alpha);
        console.log("kRate:", params.kRate);
        console.log("minBaseFee:", params.minBaseFee);
        console.log("maxBaseFee:", params.maxBaseFee);
        console.log("blockGasLimit:", params.blockGasLimit);
        console.log("inverseElasticityMultiplier:", params.inverseElasticityMultiplier);
    }

    function printConsensusParams() public view returns (IProtocolConfig.ConsensusParams memory params) {
        params = PROTOCOL_CONFIG.consensusParams();

        console.log("ProtocolConfig ConsensusParams:");
        console.log("timeoutProposeMs:", params.timeoutProposeMs);
        console.log("timeoutProposeDeltaMs:", params.timeoutProposeDeltaMs);
        console.log("timeoutPrevoteMs:", params.timeoutPrevoteMs);
        console.log("timeoutPrevoteDeltaMs:", params.timeoutPrevoteDeltaMs);
        console.log("timeoutPrecommitMs:", params.timeoutPrecommitMs);
        console.log("timeoutPrecommitDeltaMs:", params.timeoutPrecommitDeltaMs);
        console.log("timeoutRebroadcastMs:", params.timeoutRebroadcastMs);
        console.log("targetBlockTimeMs:", params.targetBlockTimeMs);
    }

    function printRewardBeneficiary() public view returns (address beneficiary) {
        beneficiary = PROTOCOL_CONFIG.rewardBeneficiary();

        console.log("ProtocolConfig RewardBeneficiary:");
        console.log("beneficiary:", beneficiary);
    }

    function printAllParams() public view {
        printFeeParams();
        console.log("");
        printConsensusParams();
        console.log("");
        printRewardBeneficiary();
    }

    // ============ Internal Helpers ============

    /**
     * @notice Gets and validates the controller key from environment
     * @return The controller private key
     */
    function _getControllerKey() internal view returns (uint256) {
        uint256 controllerKey = vm.envUint("CONTROLLER_KEY");
        require(controllerKey != 0, "CONTROLLER_KEY env var not set or is zero");
        return controllerKey;
    }

    /**
     * @notice Broadcasts a fee params update transaction
     * @param params The fee parameters to update
     */
    function _broadcastFeeParamsUpdate(IProtocolConfig.FeeParams memory params) internal {
        uint256 controllerKey = _getControllerKey();
        vm.startBroadcast(controllerKey);
        PROTOCOL_CONFIG.updateFeeParams(params);
        vm.stopBroadcast();
    }

    /**
     * @notice Broadcasts a consensus params update transaction
     * @param params The consensus parameters to update
     */
    function _broadcastConsensusParamsUpdate(IProtocolConfig.ConsensusParams memory params) internal {
        uint256 controllerKey = _getControllerKey();
        vm.startBroadcast(controllerKey);
        PROTOCOL_CONFIG.updateConsensusParams(params);
        vm.stopBroadcast();
    }

    // ============ Mutations ============

    /**
     * @notice Updates only the block gas limit while preserving other fee params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateBlockGasLimit(uint64 newBlockGasLimit) public {
        require(newBlockGasLimit > 0, "newBlockGasLimit must be > 0");

        IProtocolConfig.FeeParams memory params = PROTOCOL_CONFIG.feeParams();

        // Keep other fields unchanged; ensure invariants if uninitialized
        if (params.inverseElasticityMultiplier == 0) params.inverseElasticityMultiplier = 5000;
        if (params.maxBaseFee < params.minBaseFee) params.maxBaseFee = params.minBaseFee;

        params.blockGasLimit = newBlockGasLimit;

        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates only the target block time while preserving other consensus params. Setting to 0 disables the block time control mechanism.
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTargetBlockTime(uint16 t) public {
        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.targetBlockTimeMs = t;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates the base fee min/max bounds, preserving other fields
     * @dev Requires CONTROLLER_KEY env var; performs no input validation
     */
    function updateBaseFeeBounds(uint256 newMinBaseFee, uint256 newMaxBaseFee) public {
        IProtocolConfig.FeeParams memory params = PROTOCOL_CONFIG.feeParams();
        params.minBaseFee = newMinBaseFee;
        params.maxBaseFee = newMaxBaseFee;
        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates only the alpha parameter while preserving other fee params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateAlpha(uint64 newAlpha) public {
        IProtocolConfig.FeeParams memory params = PROTOCOL_CONFIG.feeParams();
        params.alpha = newAlpha;
        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates only the kRate parameter while preserving other fee params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateKRate(uint64 newKRate) public {
        IProtocolConfig.FeeParams memory params = PROTOCOL_CONFIG.feeParams();
        params.kRate = newKRate;
        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates only the inverse elasticity multiplier while preserving other fee params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateInverseElasticityMultiplier(uint64 newInverseElasticityMultiplier) public {
        require(newInverseElasticityMultiplier <= 10000, "inverseElasticityMultiplier must be <= 10000");

        IProtocolConfig.FeeParams memory params = PROTOCOL_CONFIG.feeParams();
        params.inverseElasticityMultiplier = newInverseElasticityMultiplier;
        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates all fee parameters at once
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateAllFeeParams(
        uint64 newAlpha,
        uint64 newKRate,
        uint64 newInverseElasticityMultiplier,
        uint256 newMinBaseFee,
        uint256 newMaxBaseFee,
        uint256 newBlockGasLimit
    ) public {
        require(newBlockGasLimit > 0, "blockGasLimit must be > 0");
        require(newInverseElasticityMultiplier <= 10000, "inverseElasticityMultiplier must be <= 10000");

        IProtocolConfig.FeeParams memory params = IProtocolConfig.FeeParams({
            alpha: newAlpha,
            kRate: newKRate,
            inverseElasticityMultiplier: newInverseElasticityMultiplier,
            minBaseFee: newMinBaseFee,
            maxBaseFee: newMaxBaseFee,
            blockGasLimit: newBlockGasLimit
        });

        _broadcastFeeParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout propose while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutPropose(uint16 newTimeoutProposeMs) public {
        require(newTimeoutProposeMs > 0, "timeoutProposeMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutProposeMs = newTimeoutProposeMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout propose delta while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutProposeDelta(uint16 newTimeoutProposeDeltaMs) public {
        require(newTimeoutProposeDeltaMs > 0, "timeoutProposeDeltaMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutProposeDeltaMs = newTimeoutProposeDeltaMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout prevote while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutPrevote(uint16 newTimeoutPrevoteMs) public {
        require(newTimeoutPrevoteMs > 0, "timeoutPrevoteMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutPrevoteMs = newTimeoutPrevoteMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout prevote delta while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutPrevoteDelta(uint16 newTimeoutPrevoteDeltaMs) public {
        require(newTimeoutPrevoteDeltaMs > 0, "timeoutPrevoteDeltaMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutPrevoteDeltaMs = newTimeoutPrevoteDeltaMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout precommit while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutPrecommit(uint16 newTimeoutPrecommitMs) public {
        require(newTimeoutPrecommitMs > 0, "timeoutPrecommitMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutPrecommitMs = newTimeoutPrecommitMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout precommit delta while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutPrecommitDelta(uint16 newTimeoutPrecommitDeltaMs) public {
        require(newTimeoutPrecommitDeltaMs > 0, "timeoutPrecommitDeltaMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutPrecommitDeltaMs = newTimeoutPrecommitDeltaMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates only the timeout rebroadcast while preserving other consensus params
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateTimeoutRebroadcast(uint16 newTimeoutRebroadcastMs) public {
        require(newTimeoutRebroadcastMs > 0, "timeoutRebroadcastMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = PROTOCOL_CONFIG.consensusParams();
        params.timeoutRebroadcastMs = newTimeoutRebroadcastMs;
        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates all consensus parameters at once
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateAllConsensusParams(
        uint16 newTimeoutProposeMs,
        uint16 newTimeoutProposeDeltaMs,
        uint16 newTimeoutPrevoteMs,
        uint16 newTimeoutPrevoteDeltaMs,
        uint16 newTimeoutPrecommitMs,
        uint16 newTimeoutPrecommitDeltaMs,
        uint16 newTimeoutRebroadcastMs,
        uint16 newTargetBlockTimeMs
    ) public {
        require(newTimeoutProposeMs > 0, "timeoutProposeMs must be > 0");
        require(newTimeoutProposeDeltaMs > 0, "timeoutProposeDeltaMs must be > 0");
        require(newTimeoutPrevoteMs > 0, "timeoutPrevoteMs must be > 0");
        require(newTimeoutPrevoteDeltaMs > 0, "timeoutPrevoteDeltaMs must be > 0");
        require(newTimeoutPrecommitMs > 0, "timeoutPrecommitMs must be > 0");
        require(newTimeoutPrecommitDeltaMs > 0, "timeoutPrecommitDeltaMs must be > 0");
        require(newTimeoutRebroadcastMs > 0, "timeoutRebroadcastMs must be > 0");

        IProtocolConfig.ConsensusParams memory params = IProtocolConfig.ConsensusParams({
            timeoutProposeMs: newTimeoutProposeMs,
            timeoutProposeDeltaMs: newTimeoutProposeDeltaMs,
            timeoutPrevoteMs: newTimeoutPrevoteMs,
            timeoutPrevoteDeltaMs: newTimeoutPrevoteDeltaMs,
            timeoutPrecommitMs: newTimeoutPrecommitMs,
            timeoutPrecommitDeltaMs: newTimeoutPrecommitDeltaMs,
            timeoutRebroadcastMs: newTimeoutRebroadcastMs,
            targetBlockTimeMs: newTargetBlockTimeMs
        });

        _broadcastConsensusParamsUpdate(params);
    }

    /**
     * @notice Updates the reward beneficiary address
     * @dev Requires CONTROLLER_KEY env var and controller role on ProtocolConfig
     */
    function updateRewardBeneficiary(address newBeneficiary) public {
        require(newBeneficiary != address(0), "beneficiary cannot be zero address");

        uint256 controllerKey = _getControllerKey();

        vm.startBroadcast(controllerKey);
        PROTOCOL_CONFIG.updateRewardBeneficiary(newBeneficiary);
        vm.stopBroadcast();
    }
}


