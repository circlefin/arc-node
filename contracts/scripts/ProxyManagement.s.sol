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

import {Script, console} from "forge-std/Script.sol";
import {AdminUpgradeableProxy} from "../src/proxy/AdminUpgradeableProxy.sol";

/// @notice Shared helpers to upgrade proxies with `upgradeTo(address)`.
contract ProxyManagement is Script {

    // ============ Helpers ============

    function printProxyInfo(address proxyAddress) public view returns (address implementation, address admin) {
        implementation = AdminUpgradeableProxy(payable(proxyAddress)).implementation();
        admin = AdminUpgradeableProxy(payable(proxyAddress)).admin();

        console.log("Proxy Info:");
        console.log("proxyAddress:", proxyAddress);
        console.log("implementation:", implementation);
        console.log("admin:", admin);
    }

    /// @dev Must be called within an active broadcast context.
    /// @dev Should be called by proxy admin.
    function upgradeProxyTo(address proxyAddress, address newImplementation) public {
        require(proxyAddress != address(0), "Proxy address is zero");
        require(proxyAddress.code.length > 0, "Proxy has no code");
        require(newImplementation != address(0), "New implementation is zero");
        require(newImplementation.code.length > 0, "New implementation has no code");

        // Call upgradeTo on proxy.
        AdminUpgradeableProxy(payable(proxyAddress)).upgradeTo(newImplementation);

        address actual = AdminUpgradeableProxy(payable(proxyAddress)).implementation();
        require(actual == newImplementation, "Implementation not updated");
    }
}
