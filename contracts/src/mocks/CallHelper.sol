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

import {IMulticall3} from "./IMulticall3.sol";

contract CallHelper {
    constructor() payable {}

    event StorageSet(address indexed sender, uint256 indexed slot, uint256 indexed value);

    function setStorage(uint256 slot, uint256 value) external payable {
        emit StorageSet(msg.sender, slot, value);
        assembly {
            sstore(slot, value)
        }
    }

    function getStorage(uint256 slot) external view returns (uint256) {
        uint256 value;
        assembly {
            value := sload(slot)
        }
        return value;
    }

    receive() external payable {}

    event ExecutionContext(address indexed sender, uint256 value);
    event ExecutionResult(bool indexed success, bytes result);

    error ExecutionFailed(bytes result);

    function transfer(address to, uint256 value) external payable {
        (bool success, bytes memory result) = to.call{value: value}(hex"");
        emit ExecutionContext(msg.sender, msg.value);
        emit ExecutionResult(success, result);
    }

    function execute(address target, bytes calldata data, uint256 value)
        external
        payable
        returns (bool success, bytes memory result)
    {
        (success, result) = target.call{value: value}(data);
        emit ExecutionResult(success, result);
        return (success, result);
    }

    function executeBatch(IMulticall3.Call3Value[] calldata calls)
        external
        payable
        returns (IMulticall3.Result[] memory results)
    {
        results = new IMulticall3.Result[](calls.length);
        for (uint256 i = 0; i < calls.length; i++) {
            IMulticall3.Call3Value memory x = calls[i];
            (bool success, bytes memory result) = x.target.call{value: x.value}(x.callData);
            emit ExecutionResult(success, result);
            results[i] = IMulticall3.Result(success, result);
            if (!x.allowFailure && !success) {
                revert ExecutionFailed(result);
            }
        }
        return results;
    }

    function staticCall(address target, bytes calldata data)
        external
        payable
        returns (bool success, bytes memory result)
    {
        (success, result) = target.staticcall(data);
        emit ExecutionResult(success, result);
        return (success, result);
    }

    function delegateCall(address target, bytes calldata data)
        external
        payable
        returns (bool success, bytes memory result)
    {
        (success, result) = target.delegatecall(data);
        emit ExecutionResult(success, result);
        return (success, result);
    }

    function callCode(address target, bytes memory data, uint256 value)
        external
        payable
        returns (bool success, bytes memory result)
    {
        assembly {
            let inLen := mload(data)
            let callSuccess := callcode(gas(), target, value, add(data, 0x20), inLen, 0, 0)
            let size := returndatasize()
            let ptr := mload(0x40)
            mstore(ptr, size)
            returndatacopy(add(ptr, 0x20), 0, size)
            success := callSuccess
            result := ptr
            mstore(0x40, add(ptr, add(size, 0x20)))
        }
        emit ExecutionResult(success, result);
        return (success, result);
    }

    error ErrorMessage(string message);

    function revertWithString(string memory message) external payable {
        revert(message);
    }

    function revertWithError(string memory message) external payable {
        revert ErrorMessage(message);
    }

    struct BlockInfo {
        address coinbase;
        uint256 timestamp;
        uint256 number;
        uint256 baseFee;
        uint256 blobBaseFee;
        uint256 prevRandao;
        uint256 gasLimit;
    }

    function getBlockInfo() external view returns (BlockInfo memory) {
        return BlockInfo({
            coinbase: block.coinbase,
            timestamp: block.timestamp,
            number: block.number,
            baseFee: block.basefee,
            blobBaseFee: block.blobbasefee,
            prevRandao: block.prevrandao,
            gasLimit: block.gaslimit
        });
    }

    struct TransactionInfo {
        uint256 gasPrice;
        address origin;
    }

    function getTxInfo() external view returns (TransactionInfo memory) {
        TransactionInfo memory info = TransactionInfo({gasPrice: tx.gasprice, origin: tx.origin});
        return info;
    }

    function blobHash(uint256 index) external view returns (bytes32) {
        return blobhash(index);
    }

    function callAndRevert(address target, bytes calldata targetCalldata) external returns (bool success, bytes memory result) {
        (success, result) = target.call(targetCalldata);
        revert("Intentional revert after call");
    }

    /// @notice Self-destructs the contract, sending any remaining balance to the target address
    function triggerSelfDestruct(address payable target) external payable {
        // solhint-disable-next-line avoid-selfdestruct
        selfdestruct(target);
    }

    /// @notice Deploys a contract using CREATE2 with the provided salt and bytecode
    function create2(bytes memory bytecode, bytes32 salt) external payable returns (address deployed) {
        uint256 amount = msg.value;
        assembly {
            // Forked from: https://github.com/OpenZeppelin/openzeppelin-contracts/blob/255e27e6d22934ddaf00c7f279039142d725382d/contracts/utils/Create2.sol#L46
            deployed := create2(amount, add(bytecode, 0x20), mload(bytecode), salt)
            if iszero(deployed) { revert(0, 0) }
        }
    }
}

interface Token {
    function transfer(address to, uint256 amount) external returns (bool);
}
