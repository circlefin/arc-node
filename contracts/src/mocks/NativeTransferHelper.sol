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

interface IBurnToken {
    function burn(uint256 amount) external;
}

/// @dev Uses selfdestruct for testing purposes.
/// The deprecation warnings are expected and can be safely ignored.
contract NativeTransferHelper {
    constructor(address payable _target, bool _selfdestruct) payable {
        if (_selfdestruct) {
            // solhint-disable-next-line avoid-selfdestruct
            selfdestruct(_target);
        }
    }

    /// @notice A function that cannot receive native token
    function cannotReceive() external {}

    /// @notice A function that can receive native token
    function canReceive() external payable {}

    /// @notice Deploys a contract using CREATE with the provided bytecode
    function create(bytes memory bytecode, uint256 createValue) external payable returns (address deployed) {
        assembly {
            deployed := create(createValue, add(bytecode, 0x20), mload(bytecode))
        }
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

    /// @notice Relays a call with value to the target, optionally requiring success
    function relay(address target, uint256 amount, bool requireSuccess, bytes calldata data) external payable {
        (bool success,) = target.call{value: amount}(data);
        require(success || !requireSuccess, "Relay reverted");
    }

    /// @notice Self-destructs the contract, sending any remaining balance to the target address
    function triggerSelfDestruct(address payable target) external payable {
        // solhint-disable-next-line avoid-selfdestruct
        selfdestruct(target);
    }

    /// @notice Executes a burn against a burnToken
    function burn(address burnToken, uint256 amount) external payable {
        IBurnToken(burnToken).burn(amount);
    }
}
