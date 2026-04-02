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

/**
 * @title IProtocolConfig
 */
interface IProtocolConfig {
    // ============ Structs ============

    struct FeeParams {
        uint64 alpha;
        uint64 kRate;
        uint64 inverseElasticityMultiplier; // target gas limit in basis points (e.g., 5000 for 50.00%, range: 0-10000)
        uint256 minBaseFee;
        uint256 maxBaseFee;
        uint256 blockGasLimit;
    }

    struct ConsensusParams {
        uint16 timeoutProposeMs;
        uint16 timeoutProposeDeltaMs;
        uint16 timeoutPrevoteMs;
        uint16 timeoutPrevoteDeltaMs;
        uint16 timeoutPrecommitMs;
        uint16 timeoutPrecommitDeltaMs;
        uint16 timeoutRebroadcastMs;
        /// @dev Target block time (setting to 0 disables the block time control mechanism)
        uint16 targetBlockTimeMs;
    }

    /* Events */

    /// @dev Emitted each time the controller updates fee parameters.
    event FeeParamsUpdated(FeeParams params);

    /// @dev Emitted each time the controller updates consensus parameters.
    event ConsensusParamsUpdated(ConsensusParams params);

    /// @dev Emitted when the reward beneficiary is reassigned.
    event RewardBeneficiaryUpdated(address indexed beneficiary);

    /* READ-ONLY API */

    /// @notice Returns the latest fee parameters.
    function feeParams() external view returns (FeeParams memory params);

    /// @notice Returns the latest consensus parameters.
    function consensusParams() external view returns (ConsensusParams memory params);

    /// @notice Returns the current reward beneficiary.
    function rewardBeneficiary() external view returns (address beneficiary);

    /* MUTATIVE API */

    /**
     * @notice Propose a new set of fee parameters.
     * @dev    Access – `onlyController` in implementation.
     * @dev    Access - `whenNotPaused` in implementation.
     * @param newParams       Complete parameter bundle.
     */
    function updateFeeParams(FeeParams calldata newParams) external;

    /**
     * @notice Propose a new set of consensus parameters.
     * @dev    Access – `onlyController` in implementation.
     * @dev    Access - `whenNotPaused` in implementation.
     * @param newParams       Complete parameter bundle.
     */
    function updateConsensusParams(ConsensusParams calldata newParams) external;

    /**
     * @notice Change the reward beneficiary address.
     * @dev    Access – `onlyController` in implementation.
     * @dev    Access - `whenNotPaused` in implementation.
     * @param newBeneficiary The new beneficiary address.
     */
    function updateRewardBeneficiary(address newBeneficiary) external;

    /**
     * @notice Update only the blockGasLimit in fee params.
     * @dev Access – `onlyController` in implementation.
     * @dev Access - `whenNotPaused` in implementation.
     * @param newBlockGasLimit The new block gas limit (must be > 0).
     */
    function updateBlockGasLimit(uint256 newBlockGasLimit) external;

    /**
     * @notice Update only the timeoutProposeMs in consensus params.
     * @dev Access – `onlyController` in implementation.
     * @dev Access - `whenNotPaused` in implementation.
     * @param newTimeoutProposeMs The new timeout for propose (must be > 0).
     */
    function updateTimeoutProposeMs(uint16 newTimeoutProposeMs) external;
}
