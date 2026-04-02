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

import {Test} from "forge-std/Test.sol";

import {Memo as MemoContract} from "../../src/memo/Memo.sol";
import {Addresses} from "../../scripts/Addresses.sol";

/// @dev Mock that simulates the callFrom precompile by forwarding the call to the target.
///      Records the sender argument so tests can verify caller preservation.
contract MockCallFrom {
    address public lastSender;

    function callFrom(address sender, address target, bytes calldata data)
        external
        returns (bool success, bytes memory returnData)
    {
        lastSender = sender;
        (success, returnData) = target.call(data);
    }
}

/// @dev Simple target contract that can succeed or revert.
contract MockTarget {
    uint256 public value;

    function setValue(uint256 v) external {
        value = v;
    }

    function reverting() external pure {
        revert("MockTarget: revert");
    }
}

contract MemoTest is Test {
    MemoContract public memoContract;
    MockTarget public target;
    address public caller;

    event BeforeMemo(uint256 indexed memoIndex);
    event Memo(
        address indexed sender,
        address indexed target,
        bytes32 callDataHash,
        bytes32 indexed memoId,
        bytes memo,
        uint256 memoIndex
    );

    function setUp() public {
        // Deploy mock and etch its bytecode at the CALL_FROM precompile address.
        // vm.etch copies bytecode only — storage lives at the precompile address itself.
        MockCallFrom mock = new MockCallFrom();
        vm.etch(Addresses.CALL_FROM, address(mock).code);

        memoContract = new MemoContract();
        target = new MockTarget();
        caller = makeAddr("caller");
    }

    function test_memo_success() public {
        bytes memory data = abi.encodeCall(MockTarget.setValue, (42));
        bytes32 memoId = keccak256("memo-1");
        bytes memory memo = "payment for invoice 123";

        vm.prank(caller);
        vm.expectEmit(true, true, true, true, address(memoContract));
        emit Memo(caller, address(target), keccak256(data), memoId, memo, 0);
        memoContract.memo(address(target), data, memoId, memo);

        assertEq(target.value(), 42);
        assertEq(memoContract.memoIndex(), 1);
    }

    function test_memo_memoIndexIncrements() public {
        bytes memory data = abi.encodeCall(MockTarget.setValue, (1));
        bytes32 memoId = keccak256("memo");
        bytes32 dataHash = keccak256(data);

        for (uint256 i = 0; i < 3; i++) {
            vm.prank(caller);
            vm.expectEmit(true, true, true, true, address(memoContract));
            emit Memo(caller, address(target), dataHash, memoId, "", i);
            memoContract.memo(address(target), data, memoId, "");
        }

        assertEq(memoContract.memoIndex(), 3);
    }

    function test_memo_callReverts() public {
        bytes memory data = abi.encodeCall(MockTarget.reverting, ());
        bytes32 memoId = keccak256("memo-fail");
        uint256 indexBefore = memoContract.memoIndex();

        vm.prank(caller);
        vm.expectRevert();
        memoContract.memo(address(target), data, memoId, "some memo");

        assertEq(memoContract.memoIndex(), indexBefore);
    }

    function test_memo_emitsBeforeMemo() public {
        bytes memory data = abi.encodeCall(MockTarget.setValue, (1));

        vm.prank(caller);
        vm.expectEmit(true, false, false, false, address(memoContract));
        emit BeforeMemo(0);
        vm.expectEmit(true, true, true, true, address(memoContract));
        emit Memo(caller, address(target), keccak256(data), bytes32(0), "", 0);
        memoContract.memo(address(target), data, bytes32(0), "");
    }

    function test_memo_emptyMemo() public {
        bytes memory data = abi.encodeCall(MockTarget.setValue, (99));
        bytes32 memoId = keccak256("empty");

        vm.prank(caller);
        vm.expectEmit(true, true, true, true, address(memoContract));
        emit Memo(caller, address(target), keccak256(data), memoId, "", 0);
        memoContract.memo(address(target), data, memoId, "");

        assertEq(target.value(), 99);
    }

    function test_memo_passesMsgSenderToPrecompile() public {
        bytes memory data = abi.encodeCall(MockTarget.setValue, (1));

        vm.prank(caller);
        memoContract.memo(address(target), data, bytes32(0), "");

        // Verify the mock recorded the correct sender (the original msg.sender, not the contract)
        assertEq(MockCallFrom(Addresses.CALL_FROM).lastSender(), caller);
    }

    function test_memo_memoIndexNotIncrementedOnRevert() public {
        bytes memory setData = abi.encodeCall(MockTarget.setValue, (1));
        bytes memory revertData = abi.encodeCall(MockTarget.reverting, ());
        bytes32 memoId = keccak256("mixed");

        // First call succeeds — memoIndex 0
        vm.prank(caller);
        memoContract.memo(address(target), setData, memoId, "first");
        assertEq(memoContract.memoIndex(), 1);

        // Second call reverts — memoIndex stays at 1
        vm.prank(caller);
        vm.expectRevert();
        memoContract.memo(address(target), revertData, memoId, "fail");
        assertEq(memoContract.memoIndex(), 1);

        // Third call succeeds — memoIndex 1
        vm.prank(caller);
        vm.expectEmit(true, true, true, true, address(memoContract));
        emit Memo(caller, address(target), keccak256(setData), memoId, "third", 1);
        memoContract.memo(address(target), setData, memoId, "third");
        assertEq(memoContract.memoIndex(), 2);
    }
}
