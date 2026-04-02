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

/// @title IMulticall3From
/// @notice Interface for Multicall3From — a sender-preserving batch-call
///         contract that routes subcalls through the callFrom precompile.
///         Mirrors the original Multicall3 API but without value-forwarding
///         methods and without payable modifiers.
interface IMulticall3From {
    struct Call {
        address target;
        bytes callData;
    }

    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    struct Result {
        bool success;
        bytes returnData;
    }

    /// @notice Aggregates calls, requiring all to succeed. Returns block
    ///         number and an array of return data.
    function aggregate(Call[] calldata calls) external returns (uint256 blockNumber, bytes[] memory returnData);

    /// @notice Aggregates calls with per-call failure flags.
    function aggregate3(Call3[] calldata calls) external returns (Result[] memory returnData);

    /// @notice Aggregates calls, requiring all to succeed. Returns block
    ///         number, block hash, and results.
    function blockAndAggregate(Call[] calldata calls)
        external
        returns (uint256 blockNumber, bytes32 blockHash, Result[] memory returnData);

    /// @notice Aggregates calls, with opt-in success requirement.
    function tryAggregate(bool requireSuccess, Call[] calldata calls)
        external
        returns (Result[] memory returnData);

    /// @notice Aggregates calls with opt-in success requirement, returning
    ///         block number, block hash, and results.
    function tryBlockAndAggregate(bool requireSuccess, Call[] calldata calls)
        external
        returns (uint256 blockNumber, bytes32 blockHash, Result[] memory returnData);

    /// @notice Returns the block hash for the given block number.
    function getBlockHash(uint256 blockNumber) external view returns (bytes32 blockHash);

    /// @notice Returns the block number of the current block.
    function getBlockNumber() external view returns (uint256 blockNumber);

    /// @notice Returns the coinbase of the current block.
    function getCurrentBlockCoinbase() external view returns (address coinbase);

    /// @notice Returns the prevrandao (difficulty) of the current block.
    function getCurrentBlockDifficulty() external view returns (uint256 difficulty);

    /// @notice Returns the gas limit of the current block.
    function getCurrentBlockGasLimit() external view returns (uint256 gaslimit);

    /// @notice Returns the timestamp of the current block.
    function getCurrentBlockTimestamp() external view returns (uint256 timestamp);

    /// @notice Returns the native token balance of the given address.
    function getEthBalance(address addr) external view returns (uint256 balance);

    /// @notice Returns the hash of the previous block.
    function getLastBlockHash() external view returns (bytes32 blockHash);

    /// @notice Returns the base fee of the current block.
    function getBasefee() external view returns (uint256 basefee);

    /// @notice Returns the chain ID.
    function getChainId() external view returns (uint256 chainid);
}
