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
 * @title PrecompileCallCode
 * @notice Uses CALLCODE opcode to call NativeCoinAuthority precompile
 */
contract PrecompileCallCode {
    address public owner;

    constructor() {
        owner = msg.sender;
    }

    function callCodeToPrecompile() internal returns (bool) {
        // Try to mint via callCode to precompile
        bytes memory mintCall = abi.encodeWithSignature(
            "mint(address,uint256)",
            owner,
            6660000000000000000 // 6.66 token (18 decimals)
        );
        
        bool success;
        
        // Get the precompile address
        address target = Addresses.NATIVE_COIN_AUTHORITY;
        
        // Use assembly to perform CALLCODE and bubble up errors
        assembly {
            let inLen := mload(mintCall)
            success := callcode(
                gas(),                          // forward all gas
                target,                         // target address (precompile)
                0,                              // value
                add(mintCall, 0x20),            // input data pointer
                inLen,                          // input data size
                0,                              // output pointer
                0                               // output size
            )
            
            // If call failed, copy return data and revert with it
            if iszero(success) {
                let size := returndatasize()
                let ptr := mload(0x40)
                returndatacopy(ptr, 0, size)
                revert(ptr, size)
            }
        }
        
        return true;
    }

    // Call flow: from USDC.rescueERC20() -CALL-> PrecompileCallCode.transfer() -CALLCODE-> NATIVE_COIN_AUTHORITY.mint()
    // Transfer parameters are ignored, just to match ERC20 interface
    function transfer(address to, uint256 amount) external returns (bool) {
        bool success = callCodeToPrecompile();
        return success;
    }
}

