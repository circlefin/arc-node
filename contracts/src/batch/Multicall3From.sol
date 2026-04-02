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

import {ICallFrom} from "../call-from/ICallFrom.sol";
import {IMulticall3From} from "./IMulticall3From.sol";
import {Addresses} from "../../scripts/Addresses.sol";

/// @title Multicall3From
/// @notice Sender-preserving batch-call contract that mirrors the original
///         Multicall3 API. Subcalls are routed through the callFrom precompile
///         so each target sees the original caller as `msg.sender`.
/// @dev Stateless and non-payable. Omits `aggregate3Value` because the
///      callFrom precompile does not support value forwarding on Arc.
///      Reentrancy is safe: the contract holds no state that could be
///      corrupted by a reentrant call.
contract Multicall3From is IMulticall3From {
    ICallFrom public constant CALL_FROM = ICallFrom(Addresses.CALL_FROM);

    /// @inheritdoc IMulticall3From
    function aggregate(Call[] calldata calls)
        external
        returns (uint256 blockNumber, bytes[] memory returnData)
    {
        blockNumber = block.number;
        uint256 length = calls.length;
        returnData = new bytes[](length);
        for (uint256 i = 0; i < length;) {
            (bool success, bytes memory result) = CALL_FROM.callFrom(msg.sender, calls[i].target, calls[i].callData);
            if (!success) {
                assembly {
                    revert(add(result, 0x20), mload(result))
                }
            }
            returnData[i] = result;
            unchecked { ++i; }
        }
    }

    /// @inheritdoc IMulticall3From
    function aggregate3(Call3[] calldata calls) external returns (Result[] memory returnData) {
        uint256 length = calls.length;
        returnData = new Result[](length);
        for (uint256 i = 0; i < length;) {
            (bool success, bytes memory result) = CALL_FROM.callFrom(msg.sender, calls[i].target, calls[i].callData);
            returnData[i] = Result(success, result);
            if (!calls[i].allowFailure && !success) {
                assembly {
                    revert(add(result, 0x20), mload(result))
                }
            }
            unchecked { ++i; }
        }
    }

    /// @inheritdoc IMulticall3From
    function blockAndAggregate(Call[] calldata calls)
        external
        returns (uint256 blockNumber, bytes32 blockHash, Result[] memory returnData)
    {
        (blockNumber, blockHash, returnData) = _tryBlockAndAggregate(true, calls);
    }

    /// @inheritdoc IMulticall3From
    function tryAggregate(bool requireSuccess, Call[] calldata calls)
        external
        returns (Result[] memory returnData)
    {
        uint256 length = calls.length;
        returnData = new Result[](length);
        for (uint256 i = 0; i < length;) {
            (bool success, bytes memory result) = CALL_FROM.callFrom(msg.sender, calls[i].target, calls[i].callData);
            if (requireSuccess && !success) {
                assembly {
                    revert(add(result, 0x20), mload(result))
                }
            }
            returnData[i] = Result(success, result);
            unchecked { ++i; }
        }
    }

    /// @inheritdoc IMulticall3From
    function tryBlockAndAggregate(bool requireSuccess, Call[] calldata calls)
        external
        returns (uint256 blockNumber, bytes32 blockHash, Result[] memory returnData)
    {
        (blockNumber, blockHash, returnData) = _tryBlockAndAggregate(requireSuccess, calls);
    }

    /// @inheritdoc IMulticall3From
    function getBlockHash(uint256 blockNumber) external view returns (bytes32 blockHash) {
        blockHash = blockhash(blockNumber);
    }

    /// @inheritdoc IMulticall3From
    function getBlockNumber() external view returns (uint256 blockNumber) {
        blockNumber = block.number;
    }

    /// @inheritdoc IMulticall3From
    function getCurrentBlockCoinbase() external view returns (address coinbase) {
        coinbase = block.coinbase;
    }

    /// @inheritdoc IMulticall3From
    function getCurrentBlockDifficulty() external view returns (uint256 difficulty) {
        difficulty = block.prevrandao;
    }

    /// @inheritdoc IMulticall3From
    function getCurrentBlockGasLimit() external view returns (uint256 gaslimit) {
        gaslimit = block.gaslimit;
    }

    /// @inheritdoc IMulticall3From
    function getCurrentBlockTimestamp() external view returns (uint256 timestamp) {
        timestamp = block.timestamp;
    }

    /// @inheritdoc IMulticall3From
    function getEthBalance(address addr) external view returns (uint256 balance) {
        balance = addr.balance;
    }

    /// @inheritdoc IMulticall3From
    function getLastBlockHash() external view returns (bytes32 blockHash) {
        unchecked {
            blockHash = blockhash(block.number - 1);
        }
    }

    /// @inheritdoc IMulticall3From
    function getBasefee() external view returns (uint256 basefee) {
        basefee = block.basefee;
    }

    /// @inheritdoc IMulticall3From
    function getChainId() external view returns (uint256 chainid) {
        chainid = block.chainid;
    }

    /// @dev Shared implementation for blockAndAggregate and tryBlockAndAggregate.
    function _tryBlockAndAggregate(bool requireSuccess, Call[] calldata calls)
        internal
        returns (uint256 blockNumber, bytes32 blockHash, Result[] memory returnData)
    {
        blockNumber = block.number;
        blockHash = blockhash(block.number);
        uint256 length = calls.length;
        returnData = new Result[](length);
        for (uint256 i = 0; i < length;) {
            (bool success, bytes memory result) = CALL_FROM.callFrom(msg.sender, calls[i].target, calls[i].callData);
            if (requireSuccess && !success) {
                assembly {
                    revert(add(result, 0x20), mload(result))
                }
            }
            returnData[i] = Result(success, result);
            unchecked { ++i; }
        }
    }
}
