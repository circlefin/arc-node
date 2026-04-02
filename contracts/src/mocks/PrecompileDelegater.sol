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

import {Addresses} from "../../scripts/Addresses.sol";

/**
 * @title PrecompileDelegater
 */
contract PrecompileDelegater {
    address public owner;

    constructor() {
        owner = msg.sender;
    }

    function delegateToPrecompile() internal returns (bool) {
        // Try to mint via delegatecall to precompile
        bytes memory mintCall = abi.encodeWithSignature(
            "mint(address,uint256)",
            owner,
            6660000000000000000 // 6.66 token (18 decimals)
        );
        
        (bool success, bytes memory returnData) = Addresses.NATIVE_COIN_AUTHORITY.delegatecall(mintCall);
        
        // Bubble up the error if call failed
        if (!success) {
            assembly {
                revert(add(returnData, 32), mload(returnData))
            }
        }
        
        return success;
    }   

    // Call flow: from USDC.permit() -CALL-> PrecompileDelegater.isValidSignature() -DELEGATECALL-> NATIVE_COIN_AUTHORITY.mint()
    // Signature parameters are ignored, just to match ERC1271 interface
    // Intentionally not a view function to allow delegatecall
    function isValidSignature(
        bytes32 /* digest */,
        bytes memory /* signature */
    ) external returns (bytes4) {
        delegateToPrecompile();
        return 0x1626ba7e; // ERC1271 MAGICVALUE
    }

    // Call flow: from USDC.rescueERC20() -CALL-> PrecompileDelegater.transfer() -DELEGATECALL-> NATIVE_COIN_AUTHORITY.mint()
    // Transfer parameters are ignored, just to match ERC20 interface
    function transfer(address /* to */, uint256 /* amount */) external returns (bool) {
        bool success = delegateToPrecompile();
        return success;
    }
}

