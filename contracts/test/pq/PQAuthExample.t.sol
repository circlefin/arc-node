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
import {PQAuthExample} from "../../src/pq/examples/PQAuthExample.sol";
import {IPQ} from "../../src/pq/IPQ.sol";
import {Precompiles} from "../../src/Precompiles.sol";

contract PQAuthExampleTest is Test {
    PQAuthExample internal example;
    address internal pq;
    address internal account = address(0xA11CE);
    bytes internal vk = new bytes(32);
    bytes internal sig = new bytes(7856);
    bytes32 internal action = keccak256("transfer 100 to bob");
    uint256 internal nonce = 1;
    uint256 internal deadline;

    function setUp() public {
        vm.warp(1000);
        example = new PQAuthExample();
        pq = Precompiles.PQ;
        deadline = block.timestamp + 1 days;
        vm.mockCall(pq, abi.encodeWithSelector(IPQ.verifySlhDsaSha2128s.selector), abi.encode(false));
    }

    function _allowCurrentDigest() internal {
        bytes memory message = abi.encodePacked(example.authDigest(account, action, nonce, deadline));
        vm.mockCall(pq, abi.encodeWithSelector(IPQ.verifySlhDsaSha2128s.selector, vk, message, sig), abi.encode(true));
    }

    function test_authorize_valid() public {
        _allowCurrentDigest();
        example.authorize(account, vk, sig, action, nonce, deadline);
        assertTrue(example.nonceUsed(account, nonce));
    }

    function test_replay_rejected_even_though_precompile_reverifies() public {
        _allowCurrentDigest();
        example.authorize(account, vk, sig, action, nonce, deadline);
        vm.expectRevert(PQAuthExample.NonceUsed.selector);
        example.authorize(account, vk, sig, action, nonce, deadline);
    }

    function test_expired_rejected() public {
        _allowCurrentDigest();
        vm.warp(deadline + 1);
        vm.expectRevert(PQAuthExample.Expired.selector);
        example.authorize(account, vk, sig, action, nonce, deadline);
    }

    function test_different_purpose_fails() public {
        _allowCurrentDigest();
        bytes32 otherAction = keccak256("transfer 100 to eve");
        vm.expectRevert(PQAuthExample.BadSignature.selector);
        example.authorize(account, vk, sig, otherAction, nonce, deadline);
    }

    function test_different_chain_fails() public {
        _allowCurrentDigest();
        vm.chainId(999);
        vm.expectRevert(PQAuthExample.BadSignature.selector);
        example.authorize(account, vk, sig, action, nonce, deadline);
    }

    function test_different_verifying_contract_fails() public {
        PQAuthExample other = new PQAuthExample();
        bytes memory message = abi.encodePacked(example.authDigest(account, action, nonce, deadline));
        vm.mockCall(pq, abi.encodeWithSelector(IPQ.verifySlhDsaSha2128s.selector, vk, message, sig), abi.encode(true));
        vm.expectRevert(PQAuthExample.BadSignature.selector);
        other.authorize(account, vk, sig, action, nonce, deadline);
    }

    function test_domain_binds_chain_and_contract() public {
        bytes32 here = example.domainSeparator();
        PQAuthExample other = new PQAuthExample();
        assertTrue(here != other.domainSeparator());
        bytes32 onThisChain = example.authDigest(account, action, nonce, deadline);
        vm.chainId(999);
        assertTrue(onThisChain != example.authDigest(account, action, nonce, deadline));
    }

    function test_bad_signature_rejected() public {
        vm.expectRevert(PQAuthExample.BadSignature.selector);
        example.authorize(account, vk, sig, action, nonce, deadline);
    }
}
