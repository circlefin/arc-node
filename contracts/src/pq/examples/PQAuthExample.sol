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

import {IPQ} from "../IPQ.sol";
import {Precompiles} from "../../Precompiles.sol";

/// @title PQAuthExample
/// @notice Domain bound authorization built over the SLH-DSA-SHA2-128s precompile.
contract PQAuthExample {
    IPQ internal constant PQ = IPQ(Precompiles.PQ);

    bytes32 private constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 private constant AUTH_TYPEHASH =
        keccak256("Authorization(address account,bytes32 action,uint256 nonce,uint256 deadline)");
    bytes32 private constant NAME_HASH = keccak256("PQAuthExample");
    bytes32 private constant VERSION_HASH = keccak256("1");

    mapping(address => mapping(uint256 => bool)) public nonceUsed;

    event Authorized(address indexed account, uint256 nonce, bytes32 action);

    error Expired();
    error NonceUsed();
    error BadSignature();

    function authorize(
        address account,
        bytes calldata vk,
        bytes calldata sig,
        bytes32 action,
        uint256 nonce,
        uint256 deadline
    ) external {
        if (block.timestamp > deadline) revert Expired();
        if (nonceUsed[account][nonce]) revert NonceUsed();

        nonceUsed[account][nonce] = true;

        bytes32 digest = authDigest(account, action, nonce, deadline);
        if (!PQ.verifySlhDsaSha2128s(vk, abi.encodePacked(digest), sig)) {
            revert BadSignature();
        }

        emit Authorized(account, nonce, action);
    }

    function domainSeparator() public view returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, NAME_HASH, VERSION_HASH, block.chainid, address(this)));
    }

    function authDigest(address account, bytes32 action, uint256 nonce, uint256 deadline)
        public
        view
        returns (bytes32)
    {
        bytes32 structHash = keccak256(abi.encode(AUTH_TYPEHASH, account, action, nonce, deadline));
        return keccak256(abi.encodePacked(hex"1901", domainSeparator(), structHash));
    }
}
