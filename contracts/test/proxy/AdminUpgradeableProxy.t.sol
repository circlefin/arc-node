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
import {AdminUpgradeableProxy} from "../../src/proxy/AdminUpgradeableProxy.sol";
import {TestImplementation} from "../../src/mocks/TestImplementation.sol";
import {CallHelper} from "../../src/mocks/CallHelper.sol";
import {ProtocolConfig} from "../../src/protocol-config/ProtocolConfig.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";

/**
 * @title AdminUpgradeableProxyTest
 * @dev Implementation-agnostic test suite for AdminUpgradeableProxy functionality
 */
contract AdminUpgradeableProxyTest is Test {
    // ============ State Variables ============

    AdminUpgradeableProxy public proxy;
    TestImplementation public implementation;
    TestImplementation public testContract;
    address public actualProxyAdmin; // The actual admin address read from proxy storage

    // Test role addresses
    address public proxyAdminAddress; // The address we set as proxy admin
    address public implementationOwner; // Owner of the TestImplementation contract
    address public user; // Regular user for testing

    // Default test parameters
    uint256 public constant DEFAULT_VALUE = 42;
    string public constant DEFAULT_NAME = "TestContract";

    // ============ Setup ============

    function setUp() public {
        // Create test addresses
        proxyAdminAddress = makeAddr("proxyAdminAddress");
        implementationOwner = makeAddr("implementationOwner");
        user = makeAddr("user");

        // Deploy implementation contract
        implementation = new TestImplementation();

        // Deploy proxy without initialization
        proxy = new AdminUpgradeableProxy(
            address(implementation),
            proxyAdminAddress,
            "" // No initialization data
        );

        // Get proxy as TestImplementation interface
        testContract = TestImplementation(payable(address(proxy)));

        // Initialize the contract through the proxy
        testContract.initialize(implementationOwner, DEFAULT_VALUE, DEFAULT_NAME);

        // Get the actual proxy admin address from ERC1967 storage
        actualProxyAdmin = address(
            uint160(
                uint256(vm.load(address(proxy), 0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103))
            )
        );
    }

    // ============ Basic Proxy Tests ============

    function test_ProxyDeployment() public view {
        // Verify proxy is deployed correctly
        assertTrue(address(proxy) != address(0));
        assertTrue(address(testContract) == address(proxy));

        // Verify proxy admin is set correctly using view function
        assertEq(proxy.admin(), proxyAdminAddress);
        assertTrue(actualProxyAdmin != address(0));
        assertEq(actualProxyAdmin, proxyAdminAddress);

        // Verify implementation is set correctly using view function
        assertEq(proxy.implementation(), address(implementation));
    }

    function test_ProxyInitialization() public view {
        // Verify TestImplementation is initialized correctly through proxy
        assertTrue(testContract.isInitialized());
        assertEq(testContract.getOwner(), implementationOwner);
        assertEq(testContract.getValue(), DEFAULT_VALUE);
        assertEq(testContract.getName(), DEFAULT_NAME);
    }

    function test_ProxyConstructorInitDataPath() public {
        // fresh implementation
        TestImplementation impl = new TestImplementation();

        // prepare init data for delegatecall in constructor
        bytes memory initData = abi.encodeWithSelector(
            TestImplementation.initialize.selector, implementationOwner, DEFAULT_VALUE, DEFAULT_NAME
        );

        // deploy with init data
        AdminUpgradeableProxy _proxy = new AdminUpgradeableProxy(address(impl), proxyAdminAddress, initData);

        // interact via the proxy as the implementation interface
        TestImplementation _impl = TestImplementation(payable(address(_proxy)));

        // verify initialized state came from the constructor delegatecall
        assertTrue(_impl.isInitialized());
        assertEq(_impl.getOwner(), implementationOwner);
        assertEq(_impl.getValue(), DEFAULT_VALUE);
        assertEq(_impl.getName(), DEFAULT_NAME);
    }

    function test_ProxyFunctionality() public {
        // Test that we can call implementation functions through the proxy
        vm.prank(implementationOwner);
        testContract.updateValue(100);
        assertEq(testContract.getValue(), 100);

        vm.prank(implementationOwner);
        testContract.updateName("Updated");
        assertEq(testContract.getName(), "Updated");
    }

    // ============ Upgrade Tests ============

    function test_ProxyUpgrade() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Verify initial state
        assertEq(testContract.getValue(), DEFAULT_VALUE);

        // Upgrade through proxy admin
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // Verify upgrade occurred but state is preserved
        assertEq(testContract.getValue(), DEFAULT_VALUE);
        assertEq(testContract.getName(), DEFAULT_NAME);
        assertTrue(testContract.isInitialized());
    }

    function test_ProxyUpgradeToAndCall() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Upgrade and call with empty data
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall(address(newImplementation), "");

        // Verify state is preserved
        assertEq(testContract.getValue(), DEFAULT_VALUE);
    }

    function test_ProxyUpgradeToAndCallWithData() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Prepare call data for updateValue (only owner can call)
        bytes memory initData = abi.encodeWithSignature("updateValue(uint256)", 999);

        // This should revert because the call is made as proxy admin, not as the implementation owner
        vm.prank(proxyAdminAddress);
        vm.expectRevert(); // Should revert because proxyAdminAddress is not the owner
        proxy.upgradeToAndCall(address(newImplementation), initData);
    }

    function test_ProxyUpgradeToAndCallSuccessful() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Change the owner to proxyAdminAddress so the call can succeed
        vm.prank(implementationOwner);
        testContract.changeOwner(proxyAdminAddress);

        // Prepare call data for updateValue (now proxyAdminAddress is owner)
        bytes memory initData = abi.encodeWithSignature("updateValue(uint256)", 999);

        // This should succeed now
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall(address(newImplementation), initData);

        // Verify state was updated
        assertEq(testContract.getValue(), 999);
    }

    function test_ProxyUpgradeToAndCallWithEther() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Prepare call data for processWithValue (payable function)
        bytes memory initData = abi.encodeWithSignature("processWithValue(bytes)", "test");

        // Give the proxy admin some ETH
        vm.deal(proxyAdminAddress, 1 ether);

        // This should succeed - calling a payable function with ETH
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall{value: 0.5 ether}(address(newImplementation), initData);

        // Verify ETH was sent to the implementation
        assertEq(address(testContract).balance, 0.5 ether);
    }

    function test_ProxyUpgradeToAndCallRejectsEtherWithEmptyData() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();

        // Give the proxy admin some ETH
        vm.deal(proxyAdminAddress, 1 ether);

        // Should revert when sending ETH with empty data
        vm.prank(proxyAdminAddress);
        vm.expectRevert(ERC1967Utils.ERC1967NonPayable.selector); // ERC1967NonPayable error from ERC1967Utils
        proxy.upgradeToAndCall{value: 0.5 ether}(address(newImplementation), "");
    }

    // ============ Access Control Tests ============

    function test_ProxyUpgradeToAndCallAccessControl() public {
        // Deploy a new implementation
        TestImplementation newImplementation = new TestImplementation();
        bytes memory emptyData = "";

        // Test that non-admin cannot call upgradeToAndCall
        // Should not revert with access control error because it falls back to implementation
        vm.prank(user);
        proxy.upgradeToAndCall(address(newImplementation), emptyData);

        // Test that admin can call upgradeToAndCall
        vm.prank(proxyAdminAddress);
        proxy.upgradeToAndCall(address(newImplementation), emptyData); // Should succeed
    }

    // ============ Transparency Tests ============

    function test_AdminCanCallImplementation() public {
        // Proxy admin CANNOT call implementation functions (blocked by transparency)
        vm.prank(address(actualProxyAdmin));
        testContract.getValue(); // Should not revert

        vm.prank(address(actualProxyAdmin));
        testContract.getName(); // Should not revert
    }

    function test_ProxyFallback() public {
        // Test that ProxyDeniedAdminAccess error is reverted for low-level calls through fallback
        vm.prank(address(actualProxyAdmin));
        (bool success1,) = address(proxy).call(abi.encodeWithSignature("getValue()"));
        assertTrue(success1);

        vm.prank(address(actualProxyAdmin));
        (bool success2,) = address(proxy).call(abi.encodeWithSignature("getName()"));
        assertTrue(success2);

        // Verify non-admin can make low-level calls successfully
        vm.prank(implementationOwner);
        (bool success, bytes memory data) = address(proxy).call(abi.encodeWithSignature("getValue()"));
        assertTrue(success);
        assertTrue(data.length > 0);
    }

    function test_NonAdminCanCallImplementation() public {
        // Non-admin users should be able to call implementation functions
        vm.prank(implementationOwner);
        uint256 value = testContract.getValue();
        assertEq(value, DEFAULT_VALUE);

        vm.prank(user);
        string memory name = testContract.getName();
        assertEq(name, DEFAULT_NAME);

        // Owner can modify state
        vm.prank(implementationOwner);
        testContract.updateValue(999);
        assertEq(testContract.getValue(), 999);
    }

    function test_NonAdminCallsAdminFunctionFallsBackToImplementation() public {
        // Test that non-admin calls to admin functions fall back to implementation
        // TestImplementation has a permissive fallback, so these calls succeed (proving delegation)

        address oldImpl = proxy.implementation();

        vm.prank(user);
        proxy.upgradeTo(address(implementation)); // This gets delegated to TestImplementation fallback
        // Implementation should NOT change because it was delegated, not executed as admin function
        assertEq(proxy.implementation(), oldImpl); // Proves it was delegated, not executed as admin

        vm.prank(implementationOwner);
        proxy.changeAdmin(user); // This also gets delegated to TestImplementation fallback
        // Admin should NOT change because it was delegated, not executed as admin function
        assertEq(proxy.admin(), proxyAdminAddress); // Proves it was delegated, not executed as admin
    }

    function test_FallbackBehaviorDemonstration() public {
        // Demonstrate the difference between admin and non-admin behavior

        // 1. Non-admin calling existing implementation function should succeed (normal delegation)
        vm.prank(user);
        uint256 value = testContract.getValue(); // This exists on TestImplementation
        assertEq(value, DEFAULT_VALUE);

        // 2. Admin calling admin function should execute as admin function (no delegation)
        address newImpl = address(new TestImplementation());
        address oldImpl = proxy.implementation();

        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(newImpl); // This executes as admin function
        assertEq(proxy.implementation(), newImpl); // Implementation DOES change

        // 3. Non-admin calling admin function should delegate to implementation (no admin effect)
        vm.prank(user);
        proxy.upgradeTo(oldImpl); // This gets delegated to TestImplementation fallback
        assertEq(proxy.implementation(), newImpl); // Implementation does NOT change (proves delegation)

        // 4. Demonstrate delegation vs admin execution
        // Admin can change admin
        vm.prank(proxyAdminAddress);
        proxy.changeAdmin(user);
        assertEq(proxy.admin(), user); // Admin DID change

        // Non-admin call gets delegated (no admin effect)
        vm.prank(implementationOwner);
        proxy.changeAdmin(proxyAdminAddress); // Gets delegated to implementation
        assertEq(proxy.admin(), user); // Admin does NOT change (proves delegation)
    }

    // ============ Storage Persistence Tests ============

    function test_StoragePersistenceAcrossUpgrade() public {
        // Modify state
        vm.prank(implementationOwner);
        testContract.updateValue(777);
        vm.prank(implementationOwner);
        testContract.updateName("Modified");

        // Verify state before upgrade
        assertEq(testContract.getValue(), 777);
        assertEq(testContract.getName(), "Modified");

        // Deploy new implementation and upgrade
        TestImplementation newImplementation = new TestImplementation();
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // Verify state persisted after upgrade
        assertEq(testContract.getValue(), 777);
        assertEq(testContract.getName(), "Modified");
        assertTrue(testContract.isInitialized());
    }

    // ============ Admin Management Tests ============

    function test_ChangeAdmin() public {
        address newAdmin = makeAddr("newAdmin");

        // Only current admin can change admin
        vm.prank(proxyAdminAddress);
        proxy.changeAdmin(newAdmin);

        // Verify admin changed
        address updatedAdmin = address(
            uint160(
                uint256(vm.load(address(proxy), 0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103))
            )
        );
        assertEq(updatedAdmin, newAdmin);

        // Old admin can still upgrade due to fallback
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(implementation));

        // New admin can upgrade
        TestImplementation newImplementation = new TestImplementation();
        vm.prank(newAdmin);
        proxy.upgradeTo(address(newImplementation)); // Should succeed
    }

    // ============ Edge Cases ============

    function test_MultipleProxiesWithSameImplementation() public {
        // Deploy another proxy with the same implementation
        AdminUpgradeableProxy proxy2 = new AdminUpgradeableProxy(address(implementation), proxyAdminAddress, "");

        TestImplementation testContract2 = TestImplementation(payable(address(proxy2)));

        // Initialize second proxy with different values
        testContract2.initialize(user, 999, "SecondProxy");

        // Verify they have independent state
        assertEq(testContract.getValue(), DEFAULT_VALUE);
        assertEq(testContract2.getValue(), 999);
        assertEq(testContract.getName(), DEFAULT_NAME);
        assertEq(testContract2.getName(), "SecondProxy");

        // Verify they can be upgraded independently
        TestImplementation newImplementation = new TestImplementation();
        vm.prank(proxyAdminAddress);
        proxy.upgradeTo(address(newImplementation));

        // First proxy upgraded, second still on old implementation
        assertEq(testContract.getValue(), DEFAULT_VALUE); // State preserved
        assertEq(testContract2.getValue(), 999); // State preserved
    }

    function test_ImplementationViewFunction() public {
        // Deploy implementations
        TestImplementation impl1 = new TestImplementation();
        TestImplementation impl2 = new TestImplementation();

        // Deploy proxy
        AdminUpgradeableProxy testProxy = new AdminUpgradeableProxy(address(impl1), proxyAdminAddress, "");

        // Verify implementation() returns correct address
        assertEq(testProxy.implementation(), address(impl1));

        // Non-admin should be able to call view function
        vm.prank(implementationOwner);
        assertEq(testProxy.implementation(), address(impl1));

        // Admin should also be able to call view function
        vm.prank(proxyAdminAddress);
        assertEq(testProxy.implementation(), address(impl1));

        // Upgrade and verify new implementation
        vm.prank(proxyAdminAddress);
        testProxy.upgradeTo(address(impl2));

        assertEq(testProxy.implementation(), address(impl2));
    }

    function test_AdminViewFunction() public {
        // Deploy implementation
        TestImplementation impl = new TestImplementation();

        // Deploy proxy
        AdminUpgradeableProxy testProxy = new AdminUpgradeableProxy(address(impl), proxyAdminAddress, "");

        // Verify admin() returns correct address
        assertEq(testProxy.admin(), proxyAdminAddress);

        // Non-admin should be able to call view function
        vm.prank(implementationOwner);
        assertEq(testProxy.admin(), proxyAdminAddress);

        // Admin should also be able to call view function
        vm.prank(proxyAdminAddress);
        assertEq(testProxy.admin(), proxyAdminAddress);

        // Change admin and verify new admin
        address newAdmin = makeAddr("newAdmin");
        vm.prank(proxyAdminAddress);
        testProxy.changeAdmin(newAdmin);

        assertEq(testProxy.admin(), newAdmin);
    }

    function test_ViewFunctionsAccessibility() public {
        // Deploy implementation
        TestImplementation impl = new TestImplementation();

        // Deploy proxy
        AdminUpgradeableProxy testProxy = new AdminUpgradeableProxy(address(impl), proxyAdminAddress, "");

        // Test that anyone can call view functions
        address randomUser = makeAddr("randomUser");

        vm.prank(randomUser);
        assertEq(testProxy.implementation(), address(impl));

        vm.prank(randomUser);
        assertEq(testProxy.admin(), proxyAdminAddress);

        // Verify these are truly view functions (don't change state)
        address implBefore = testProxy.implementation();
        address adminBefore = testProxy.admin();

        // Multiple calls should return same values
        assertEq(testProxy.implementation(), implBefore);
        assertEq(testProxy.admin(), adminBefore);
    }

    function test_NativeTransferFailsWithoutPayableReceiveImplementation() public {
        // Deploy a non-payable implementation (no receive/fallback)
        ProtocolConfig impl = new ProtocolConfig();
        address proxyAdmin = makeAddr("protocolProxyAdmin");
        AdminUpgradeableProxy protocolProxy = new AdminUpgradeableProxy(address(impl), proxyAdmin, "");

        // Fund a user and attempt to send ETH while calling a view function (rewardBeneficiary)
        address sender = makeAddr("nativeSenderWithViewSelector");
        vm.deal(sender, 1 ether);

        vm.prank(sender);
        (bool success, bytes memory revertData) = address(protocolProxy).call{value: 0.1 ether}("");

        // Should fail because the function is non-payable and lacks receive/fallback payable handler
        assertFalse(success, "native transfer with view selector should fail");
        assertEq(address(protocolProxy).balance, 0 ether, "proxy should not hold ETH");
    }

    function test_NativeTransferSucceedsWithPayableReceiveImplementation() public {
        // Deploy an implementation that has a payable receive
        CallHelper impl = new CallHelper();
        address proxyAdmin = makeAddr("helperProxyAdmin");
        AdminUpgradeableProxy helperProxy = new AdminUpgradeableProxy(address(impl), proxyAdmin, "");

        // Fund a user and send ETH with empty calldata to the proxy
        address sender = makeAddr("nativeSenderWithReceive");
        vm.deal(sender, 1 ether);

        vm.prank(sender);
        (bool success,) = address(helperProxy).call{value: 0.2 ether}("");

        // Call should succeed and ETH should be held by the proxy (delegatecall keeps balance on proxy)
        assertTrue(success, "native transfer should succeed via payable receive");
        assertEq(address(helperProxy).balance, 0.2 ether, "proxy should hold transferred ETH");
    }

    function test_PayableVsNonPayableFunctionOnImplementation() public {
        // Deploy an implementation that has both payable and non-payable entrypoints
        CallHelper impl = new CallHelper();
        address proxyAdmin = makeAddr("helperProxyAdmin2");
        AdminUpgradeableProxy helperProxy = new AdminUpgradeableProxy(address(impl), proxyAdmin, "");

        address sender = makeAddr("payableVsNonPayable");
        vm.deal(sender, 1 ether);

        // Payable function: setStorage is payable, should accept value
        bytes memory payableCall = abi.encodeWithSelector(CallHelper.setStorage.selector, uint256(1), uint256(123));
        vm.prank(sender);
        (bool payableSuccess,) = address(helperProxy).call{value: 0.15 ether}(payableCall);
        assertTrue(payableSuccess, "payable function should accept ETH");
        assertEq(address(helperProxy).balance, 0.15 ether, "proxy should hold only accepted ETH");

        // Non-payable function: getStorage is view (non-payable), should reject value
        bytes memory nonPayableCall = abi.encodeWithSelector(CallHelper.getStorage.selector, uint256(1));
        vm.prank(sender);
        (bool nonPayableSuccess, bytes memory revertData) =
            address(helperProxy).call{value: 0.05 ether}(nonPayableCall);

        assertFalse(nonPayableSuccess, "non-payable function should reject ETH");

        // Only the successful payable call's value should remain on the proxy
        assertEq(address(helperProxy).balance, 0.15 ether, "proxy should hold only accepted ETH");
    }
}
