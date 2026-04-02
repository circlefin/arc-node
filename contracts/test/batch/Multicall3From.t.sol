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

import {Multicall3From} from "../../src/batch/Multicall3From.sol";
import {IMulticall3From} from "../../src/batch/IMulticall3From.sol";
import {Addresses} from "../../scripts/Addresses.sol";

/// @dev Mock that mimics the callFrom precompile: records every sender and target
///      passed to callFrom, then forwards the call to the real target via target.call(data).
///      Records are stored in arrays so tests can assert the core invariant (sender
///      preservation) across every call in a batch, not just the last one.
contract MockCallFrom {
    address[] public senders;
    address[] public targets;

    function callFrom(address sender, address target, bytes calldata data)
        external
        returns (bool success, bytes memory returnData)
    {
        senders.push(sender);
        targets.push(target);
        (success, returnData) = target.call(data);
    }

    function callCount() external view returns (uint256) {
        return senders.length;
    }

    function getSender(uint256 i) external view returns (address) {
        return senders[i];
    }

    function getTarget(uint256 i) external view returns (address) {
        return targets[i];
    }
}

/// @dev Simple target contract for testing.
contract MockTarget {
    uint256 public value;

    function setValue(uint256 v) external {
        value = v;
    }

    function getValue() external view returns (uint256) {
        return value;
    }

    function alwaysReverts() external pure {
        revert("MockTarget: always reverts");
    }

    error CustomError(uint256 code);

    function revertsWithCustomError(uint256 code) external pure {
        revert CustomError(code);
    }
}

contract Multicall3FromTest is Test {
    Multicall3From multicall;
    MockCallFrom mockCallFrom;
    MockTarget target;

    function setUp() public {
        multicall = new Multicall3From();
        mockCallFrom = new MockCallFrom();
        target = new MockTarget();

        // Place mock callFrom bytecode at the well-known precompile address.
        // vm.etch copies only bytecode, not storage. Storage starts at zero-value
        // defaults, which is acceptable because MockCallFrom uses dynamic arrays
        // that start empty.
        vm.etch(Addresses.CALL_FROM, address(mockCallFrom).code);
    }

    // ======================== CALL_FROM constant ========================

    function test_callFromConstant() public view {
        assertEq(address(multicall.CALL_FROM()), Addresses.CALL_FROM);
    }

    // ======================== aggregate3 ========================

    function test_aggregate3_singleCallSuccess() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](1);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (42))
        });

        IMulticall3From.Result[] memory results = multicall.aggregate3(calls);

        assertEq(results.length, 1);
        assertTrue(results[0].success);
        assertEq(target.value(), 42);
    }

    function test_aggregate3_preservesSender() public {
        address caller = address(0xBEEF);

        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](1);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (1))
        });

        vm.prank(caller);
        multicall.aggregate3(calls);

        // Core invariant: the mock records the sender passed to callFrom.
        // It must be the original caller, not the Multicall3From contract address.
        MockCallFrom mock = MockCallFrom(Addresses.CALL_FROM);
        assertEq(mock.callCount(), 1);
        assertEq(mock.getSender(0), caller);
        assertEq(mock.getTarget(0), address(target));
    }

    function test_aggregate3_multipleCalls() public {
        MockTarget target2 = new MockTarget();

        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](2);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (10))
        });
        calls[1] = IMulticall3From.Call3({
            target: address(target2),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (20))
        });

        IMulticall3From.Result[] memory results = multicall.aggregate3(calls);

        assertEq(results.length, 2);
        assertTrue(results[0].success);
        assertTrue(results[1].success);
        assertEq(target.value(), 10);
        assertEq(target2.value(), 20);
    }

    function test_aggregate3_emptyBatch() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](0);

        IMulticall3From.Result[] memory results = multicall.aggregate3(calls);

        assertEq(results.length, 0);
    }

    function test_aggregate3_allowFailureTrue_callReverts() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](2);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: true,
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });
        calls[1] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (99))
        });

        IMulticall3From.Result[] memory results = multicall.aggregate3(calls);

        assertEq(results.length, 2);
        assertFalse(results[0].success);
        assertTrue(results[1].success);
        assertEq(target.value(), 99);
    }

    function test_aggregate3_allowFailureFalse_callReverts() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](1);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        vm.expectRevert("MockTarget: always reverts");
        multicall.aggregate3(calls);
    }

    function test_aggregate3_propagatesCustomError() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](1);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.revertsWithCustomError, (42))
        });

        vm.expectRevert(abi.encodeWithSelector(MockTarget.CustomError.selector, 42));
        multicall.aggregate3(calls);
    }

    function test_aggregate3_senderPreservedAcrossBatch() public {
        address caller = address(0xCAFE);
        MockTarget target2 = new MockTarget();

        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](2);
        calls[0] = IMulticall3From.Call3({
            target: address(target),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (1))
        });
        calls[1] = IMulticall3From.Call3({
            target: address(target2),
            allowFailure: false,
            callData: abi.encodeCall(MockTarget.setValue, (2))
        });

        vm.prank(caller);
        multicall.aggregate3(calls);

        // Verify sender is preserved for every call in the batch, not just the last.
        MockCallFrom mock = MockCallFrom(Addresses.CALL_FROM);
        assertEq(mock.callCount(), 2);
        assertEq(mock.getSender(0), caller);
        assertEq(mock.getSender(1), caller);
        assertEq(mock.getTarget(0), address(target));
        assertEq(mock.getTarget(1), address(target2));
    }

    function test_aggregate3_rejectsValue() public {
        IMulticall3From.Call3[] memory calls = new IMulticall3From.Call3[](0);

        // aggregate3 is non-payable, sending value should revert
        (bool success,) = address(multicall).call{value: 1 ether}(
            abi.encodeCall(Multicall3From.aggregate3, (calls))
        );
        assertFalse(success);
    }

    // ======================== aggregate ========================

    function test_aggregate_singleCall() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.setValue, (77))
        });

        (uint256 blockNumber, bytes[] memory returnData) = multicall.aggregate(calls);

        assertEq(blockNumber, block.number);
        assertEq(returnData.length, 1);
        assertEq(target.value(), 77);
    }

    function test_aggregate_revertsOnFailure() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        vm.expectRevert("MockTarget: always reverts");
        multicall.aggregate(calls);
    }

    function test_aggregate_preservesSender() public {
        address caller = address(0xDEAD);

        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.setValue, (5))
        });

        vm.prank(caller);
        multicall.aggregate(calls);

        MockCallFrom mock = MockCallFrom(Addresses.CALL_FROM);
        assertEq(mock.callCount(), 1);
        assertEq(mock.getSender(0), caller);
    }

    // ======================== tryAggregate ========================

    function test_tryAggregate_requireSuccessTrue_reverts() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        vm.expectRevert("MockTarget: always reverts");
        multicall.tryAggregate(true, calls);
    }

    function test_tryAggregate_requireSuccessFalse_doesNotRevert() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](2);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });
        calls[1] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.setValue, (50))
        });

        IMulticall3From.Result[] memory results = multicall.tryAggregate(false, calls);

        assertEq(results.length, 2);
        assertFalse(results[0].success);
        assertTrue(results[1].success);
        assertEq(target.value(), 50);
    }

    // ======================== blockAndAggregate ========================

    function test_blockAndAggregate_returnsBlockInfo() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.setValue, (33))
        });

        (uint256 blockNumber, bytes32 blockHash, IMulticall3From.Result[] memory results) =
            multicall.blockAndAggregate(calls);

        assertEq(blockNumber, block.number);
        assertEq(blockHash, blockhash(block.number));
        assertEq(results.length, 1);
        assertTrue(results[0].success);
        assertEq(target.value(), 33);
    }

    function test_blockAndAggregate_revertsOnFailure() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        vm.expectRevert("MockTarget: always reverts");
        multicall.blockAndAggregate(calls);
    }

    // ======================== tryBlockAndAggregate ========================

    function test_tryBlockAndAggregate_requireSuccessFalse() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        (uint256 blockNumber, bytes32 blockHash, IMulticall3From.Result[] memory results) =
            multicall.tryBlockAndAggregate(false, calls);

        assertEq(blockNumber, block.number);
        assertEq(blockHash, blockhash(block.number));
        assertEq(results.length, 1);
        assertFalse(results[0].success);
    }

    function test_tryBlockAndAggregate_requireSuccessTrue_reverts() public {
        IMulticall3From.Call[] memory calls = new IMulticall3From.Call[](1);
        calls[0] = IMulticall3From.Call({
            target: address(target),
            callData: abi.encodeCall(MockTarget.alwaysReverts, ())
        });

        vm.expectRevert("MockTarget: always reverts");
        multicall.tryBlockAndAggregate(true, calls);
    }

    // ======================== View helpers ========================

    function test_getBlockNumber() public view {
        assertEq(multicall.getBlockNumber(), block.number);
    }

    function test_getCurrentBlockTimestamp() public view {
        assertEq(multicall.getCurrentBlockTimestamp(), block.timestamp);
    }

    function test_getCurrentBlockGasLimit() public view {
        assertEq(multicall.getCurrentBlockGasLimit(), block.gaslimit);
    }

    function test_getCurrentBlockCoinbase() public view {
        assertEq(multicall.getCurrentBlockCoinbase(), block.coinbase);
    }

    function test_getCurrentBlockDifficulty() public view {
        assertEq(multicall.getCurrentBlockDifficulty(), block.prevrandao);
    }

    function test_getBasefee() public view {
        assertEq(multicall.getBasefee(), block.basefee);
    }

    function test_getChainId() public view {
        assertEq(multicall.getChainId(), block.chainid);
    }

    function test_getBlockHash() public view {
        assertEq(multicall.getBlockHash(block.number), blockhash(block.number));
    }

    function test_getLastBlockHash() public view {
        assertEq(multicall.getLastBlockHash(), blockhash(block.number - 1));
    }

    function test_getEthBalance() public view {
        assertEq(multicall.getEthBalance(address(this)), address(this).balance);
    }
}
