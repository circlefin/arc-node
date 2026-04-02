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

import {Test} from "forge-std/Test.sol";

import {CallHelper} from "../../src/mocks/CallHelper.sol";
import {IMulticall3} from "../../src/mocks/IMulticall3.sol";
import {ArtifactHelper} from "../../scripts/ArtifactHelper.s.sol";

contract CallHelperTest is Test {
    ArtifactHelper artifactHelper;
    address eoa;
    CallHelper A;
    CallHelper B;
    CallHelper C;

    function setUp() public {
        artifactHelper = new ArtifactHelper();
        artifactHelper.loadManifest("assets/artifacts/manifest.json");
        ArtifactHelper.OneTimeAddressDeployment memory deployment =
            artifactHelper.loadOneTimeAddressDeployment("Multicall3");
        artifactHelper.deployOneTimeAddressContract(deployment);

        eoa = makeAddr("EOA");
        A = new CallHelper();
        B = new CallHelper();
        C = new CallHelper();
    }

    function testGetSetValueAndStorage() public {
        A.setStorage(0, 193);
        assertEq(A.getStorage(0), 193);
    }

    function testExecute() public {
        // EOA -> A
        vm.deal(eoa, 100);
        vm.prank(eoa);
        vm.expectEmit(address(A));
        emit CallHelper.ExecutionResult(true, hex"");
        bytes memory data;
        (bool success, bytes memory returnValue) = A.execute{value: 30}(address(A), data, 0);
        assertTrue(success);
        assertEq(returnValue, hex"");
        assertEq(address(A).balance, 30);
    }

    function testExecuteBatch() public {
        // EOA -> A
        //   A -> B and C
        vm.deal(address(A), 10);
        vm.prank(eoa);
        vm.expectEmit(true, true, false, false, address(A));
        emit CallHelper.ExecutionResult(false, hex"");
        vm.expectEmit(true, true, true, false, address(A));
        emit CallHelper.ExecutionResult(true, hex"");
        IMulticall3.Call3Value[] memory calls = new IMulticall3.Call3Value[](2);
        calls[0] = IMulticall3.Call3Value(address(B), true, 0, abi.encodeWithSelector(B.revertWithString.selector, "B"));
        calls[1] = IMulticall3.Call3Value(address(C), true, 10, hex"");
        (IMulticall3.Result[] memory res) = A.executeBatch(calls);
        assertEq(res[0].success, false);
        assertEq(res[1].success, true);
    }

    function testStaticCall() public {
        // EOA -> A.staticCall
        //   A -> B.execute
        //     B 10 -> C
        vm.deal(address(B), 10);
        vm.prank(eoa);
        vm.expectEmit(true, true, true, false, address(A));
        emit CallHelper.ExecutionResult(false, hex"");
        (bool success, bytes memory returnValue) =
            A.staticCall(address(B), abi.encodeWithSelector(B.execute.selector, address(C), 10));
        assertFalse(success);
        assertEq(returnValue, hex"");
        assertEq(address(C).balance, 0);
    }

    function testDelegateCall() public {
        // EOA -> A.delegateCall
        //   EOA -> A delegate A.receive
        vm.prank(eoa);
        vm.expectEmit(address(A));
        emit CallHelper.ExecutionResult(true, hex"");
        bytes memory data;
        (bool success, bytes memory returnValue) = A.delegateCall(address(A), data);
        assertTrue(success);
        assertEq(returnValue, hex"");
    }

    function testDelegateCallSetValue() public {
        // EOA -> A.delegateCall
        //   EOA -> A delegate B.setStorage
        vm.prank(eoa);
        vm.expectEmit(address(A));
        emit CallHelper.ExecutionResult(true, hex"");
        (bool success, bytes memory returnValue) =
            A.delegateCall(address(B), abi.encodeWithSelector(B.setStorage.selector, 0, 13));
        assertTrue(success);
        assertEq(returnValue, hex"");
        assertEq(A.getStorage(0), 13);
        assertEq(B.getStorage(0), 0);
    }

    function testBlockInfo() public view {
        CallHelper.BlockInfo memory blockInfo = A.getBlockInfo();
        assertEq(blockInfo.coinbase, address(0));
        assertEq(blockInfo.timestamp, 1);
        assertEq(blockInfo.number, 1);
        assertEq(blockInfo.baseFee, 0);
        assertEq(blockInfo.blobBaseFee, 1);
        assertEq(blockInfo.prevRandao, 0);
        assertEq(blockInfo.gasLimit, 1073741824);
    }

    function testTxInfo() public {
        vm.prank(eoa);
        CallHelper.TransactionInfo memory txInfo = A.getTxInfo();
        assertEq(txInfo.gasPrice, 0);
        assertEq(txInfo.origin, tx.origin);
    }

    function testBlockHash() public view {
        bytes32 hash = IMulticall3(address(0xcA11bde05977b3631167028862bE2a173976CA11)).getBlockHash(100);
        assertEq(hash, bytes32(0));
    }
}
