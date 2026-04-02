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

import {Controller} from "./roles/Controller.sol";
import {Pausable} from "../common/roles/Pausable.sol";
import {IProtocolConfig} from "./interfaces/IProtocolConfig.sol";

/**
 * @title ProtocolConfig
 * @dev Contract for managing protocol configuration parameters including fee settings
 * @dev This contract is designed to be used with upgradeable transparent proxies
 */
contract ProtocolConfig is Controller, Pausable, IProtocolConfig {
    // ============ Errors ============

    /// @notice Thrown when contract is already initialized
    error AlreadyInitialized();

    /// @notice Thrown when alpha parameter is invalid (must be <= 100)
    error InvalidAlpha();

    /// @notice Thrown when kRate parameter is invalid (must be <= 10000)
    error InvalidKRate();

    /// @notice Thrown when minBaseFee is greater than maxBaseFee
    error InvalidBaseFeeRange();

    /// @notice Thrown when blockGasLimit is zero
    error InvalidBlockGasLimit();

    /// @notice Thrown when inverseElasticityMultiplier exceeds 10000
    error InvalidInverseElasticityMultiplier();

    /// @notice Thrown when timeoutProposeMs is zero
    error InvalidTimeoutProposeMs();

    /// @notice Thrown when timeoutProposeDeltaMs is zero
    error InvalidTimeoutProposeDeltaMs();

    /// @notice Thrown when timeoutPrevoteMs is zero
    error InvalidTimeoutPrevoteMs();

    /// @notice Thrown when timeoutPrevoteDeltaMs is zero
    error InvalidTimeoutPrevoteDeltaMs();

    /// @notice Thrown when timeoutPrecommitMs is zero
    error InvalidTimeoutPrecommitMs();

    /// @notice Thrown when timeoutPrecommitDeltaMs is zero
    error InvalidTimeoutPrecommitDeltaMs();

    /// @notice Thrown when timeoutRebroadcastMs is zero
    error InvalidTimeoutRebroadcastMs();

    // ============ Storage ============

    /// @custom:storage-location erc7201:arc.storage.ProtocolConfig
    struct ProtocolConfigStorage {
        FeeParams feeParams;
        address rewardBeneficiary;
        ConsensusParams consensusParams;
    }

    // keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfig")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant PROTOCOL_CONFIG_STORAGE_LOCATION =
        0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200;

    /**
     * @dev Returns the storage pointer for ProtocolConfig state
     */
    function _getProtocolConfigStorage() private pure returns (ProtocolConfigStorage storage $) {
        assembly {
            $.slot := PROTOCOL_CONFIG_STORAGE_LOCATION
        }
    }

    // ============ Constructor ============

    /**
     * @dev Constructor for implementation contract - disables initializers to prevent misuse
     */
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        // Verify storage slot calculation is correct
        assert(
            PROTOCOL_CONFIG_STORAGE_LOCATION
                == keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfig")) - 1)) & ~bytes32(uint256(0xff))
        );
        // Disable initializers on implementation contract
        _disableInitializers();
    }

    // ============ Read-Only Functions ============

    /**
     * @notice Returns the current fee parameters
     * @return The current fee parameters struct
     */
    function feeParams() external view override returns (FeeParams memory) {
        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        return $.feeParams;
    }

    /**
     * @notice Returns the current consensus parameters
     * @return The current consensus parameters struct
     */
    function consensusParams() external view override returns (ConsensusParams memory) {
        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        return $.consensusParams;
    }

    /**
     * @notice Returns the current reward beneficiary address
     * @return The address of the current reward beneficiary
     */
    function rewardBeneficiary() external view override returns (address) {
        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        return $.rewardBeneficiary;
    }

    // ============ Mutative Functions ============

    /**
     * @notice Updates the fee parameters
     * @dev Only callable by controller when not paused
     * @param newParams The new fee parameters
     */
    function updateFeeParams(FeeParams calldata newParams) external override onlyController whenNotPaused {
        // Validate parameters
        require(newParams.alpha <= 100, InvalidAlpha());
        require(newParams.kRate <= 10000, InvalidKRate());
        require(newParams.minBaseFee <= newParams.maxBaseFee, InvalidBaseFeeRange());
        require(newParams.blockGasLimit > 0, InvalidBlockGasLimit());
        require(newParams.inverseElasticityMultiplier <= 10000, InvalidInverseElasticityMultiplier());

        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        $.feeParams = newParams;
        emit FeeParamsUpdated(newParams);
    }

    /**
     * @notice Updates the consensus parameters
     * @dev Only callable by controller when not paused
     * @param newParams The new consensus parameters
     */
    function updateConsensusParams(ConsensusParams calldata newParams) external override onlyController whenNotPaused {
        // Validate parameters
        require(newParams.timeoutProposeMs > 0, InvalidTimeoutProposeMs());
        require(newParams.timeoutProposeDeltaMs > 0, InvalidTimeoutProposeDeltaMs());
        require(newParams.timeoutPrevoteMs > 0, InvalidTimeoutPrevoteMs());
        require(newParams.timeoutPrevoteDeltaMs > 0, InvalidTimeoutPrevoteDeltaMs());
        require(newParams.timeoutPrecommitMs > 0, InvalidTimeoutPrecommitMs());
        require(newParams.timeoutPrecommitDeltaMs > 0, InvalidTimeoutPrecommitDeltaMs());
        require(newParams.timeoutRebroadcastMs > 0, InvalidTimeoutRebroadcastMs());

        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        $.consensusParams = newParams;
        emit ConsensusParamsUpdated(newParams);
    }

    /**
     * @notice Updates the reward beneficiary address
     * @dev Only callable by controller when not paused
     * @param newBeneficiary The new reward beneficiary address
     */
    function updateRewardBeneficiary(address newBeneficiary) external override onlyController whenNotPaused {
        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        $.rewardBeneficiary = newBeneficiary;
        emit RewardBeneficiaryUpdated(newBeneficiary);
    }

    /**
     * @notice Updates only the blockGasLimit parameter
     * @dev Only callable by controller when not paused
     * @param newBlockGasLimit The new block gas limit (must be > 0)
     */
    function updateBlockGasLimit(uint256 newBlockGasLimit) external override onlyController whenNotPaused {
        require(newBlockGasLimit > 0, InvalidBlockGasLimit());

        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        $.feeParams.blockGasLimit = newBlockGasLimit;
        emit FeeParamsUpdated($.feeParams);
    }

    /**
     * @notice Updates only the timeoutProposeMs parameter
     * @dev Only callable by controller when not paused
     * @param newTimeoutProposeMs The new timeout propose in milliseconds (must be > 0)
     */
    function updateTimeoutProposeMs(uint16 newTimeoutProposeMs) external override onlyController whenNotPaused {
        require(newTimeoutProposeMs > 0, InvalidTimeoutProposeMs());

        ProtocolConfigStorage storage $ = _getProtocolConfigStorage();
        $.consensusParams.timeoutProposeMs = newTimeoutProposeMs;
        emit ConsensusParamsUpdated($.consensusParams);
    }
}
