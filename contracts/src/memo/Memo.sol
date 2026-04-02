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
import {IMemo} from "./IMemo.sol";
import {Addresses} from "../../scripts/Addresses.sol";

/**
 * @title Memo
 * @notice Wraps the callFrom precompile to attach memo metadata to subcalls.
 * @dev Not upgradeable, no proxy. Deployed at runtime via CREATE2.
 *      The callFrom precompile enforces its own allowlist — this contract does not add access control.
 */
contract Memo is IMemo {
    /// @inheritdoc IMemo
    uint256 public memoIndex;

    /// @notice The callFrom precompile used to forward subcalls with caller preservation.
    ICallFrom public constant CALL_FROM = ICallFrom(Addresses.CALL_FROM);

    /// @inheritdoc IMemo
    function memo(address target, bytes calldata data, bytes32 memoId, bytes calldata memo) external {
        // Pre-increment ensures unique indices under reentrancy; each call captures its own local index.
        uint256 currentMemoIndex = memoIndex++;
        emit BeforeMemo(currentMemoIndex);
        (bool success, bytes memory returnData) = CALL_FROM.callFrom(msg.sender, target, data);
        if (!success) revert MemoFailed(returnData);
        emit Memo(msg.sender, target, keccak256(data), memoId, memo, currentMemoIndex);
    }
}
