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

/// @title IMemo
/// @notice Interface for the Memo contract.
/// @dev EOA-only: {memo} invokes the callFrom precompile, which requires the
///      sender argument (`msg.sender` of Memo) to equal the precompile caller
///      or `tx.origin`. A contract caller is neither, so callFrom reverts and
///      the entire call reverts without raising {MemoFailed}.
///
///      Target-contract reverts (callFrom returning `success=false`) are
///      re-raised as {MemoFailed} with the target's revert bytes.
///
/// @dev Canonical entry point is {memo} with signature
///      `memo(address,bytes,bytes32,bytes)`. There is no `callWithMemo` function.
///      Calling a wrong selector reverts with empty data and will also make
///      `eth_estimateGas` / `eth_call` fail — that is an ABI mismatch, not a
///      CallFrom simulation bug. Pass an EOA as `from` when estimating.
///
/// @dev {memo} is state-changing (`memoIndex++`). STATICCALL into Memo is
///      rejected; ordinary `eth_call` / `eth_estimateGas` (non-static top-level
///      simulation) succeeds with the correct ABI.
interface IMemo {
    /// @notice Thrown when the call via callFrom fails.
    error MemoFailed(bytes returnData);

    /// @notice Emitted before the subcall is executed, carrying the memoIndex that will be used.
    event BeforeMemo(uint256 indexed memoIndex);

    /// @notice Emitted after a successful subcall with the associated memo metadata.
    event Memo(
        address indexed sender,
        address indexed target,
        bytes32 callDataHash,
        bytes32 indexed memoId,
        bytes memo,
        uint256 memoIndex
    );

    /// @notice Returns the current memo index.
    function memoIndex() external view returns (uint256);

    /// @notice Executes a subcall via the callFrom precompile and emits memo metadata.
    /// @dev EOA-only. See {IMemo}.
    /// @param target The address to call via the precompile.
    /// @param data The calldata to forward to the target.
    /// @param memoId A caller-supplied identifier for the memo.
    /// @param memoData Arbitrary memo bytes attached to the subcall (not `string`).
    function memo(address target, bytes calldata data, bytes32 memoId, bytes calldata memoData) external;
}
