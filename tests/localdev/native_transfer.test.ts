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

import { balancesSnapshot, NativeCoinAuthority, ReceiptVerifier, NativeTransferHelper, getClients } from '../helpers'
import { parseEther, keccak256, getCreate2Address, getCreateAddress } from 'viem'
import { expect } from 'chai'
import { callHelperArtifact } from '../helpers/CallHelper'

describe('native transfer', () => {
  let nativeTransferHelper: NativeTransferHelper
  let altNativeTransferHelper: NativeTransferHelper

  const clients = async () => {
    const { client, sender, receiver, admin } = await getClients()
    const totalSupply = async () => NativeCoinAuthority.totalSupply(client)
    return { client, admin, sender, receiver, totalSupply }
  }

  before(async () => {
    const { client, admin } = await clients()

    nativeTransferHelper = await NativeTransferHelper.deploy(admin, client, 0n)
    // Deploy a second contract to assist with testing chained execution
    altNativeTransferHelper = await NativeTransferHelper.deploy(admin, client, 0n)
  })

  it('transfer', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const amount = parseEther('0.0000001')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      totalSupply,
    })

    const receipt = await sender
      .sendTransaction({ to: receiver.account.address, value: amount })
      .then(ReceiptVerifier.waitSuccess)

    // Zero5: cold SLOAD = 2100 for blocklist check
    // Value transfer: base 21000 + caller check 2100 + recipient check 2100 = 25200
    receipt.verifyGasUsed(25200n).verifyEvents((ev) => {
      ev.expectCount(1).expectNativeTransfer({ from: sender, to: receiver, amount })
    })

    await balances
      .increase({ receiver: amount })
      .decrease({ sender: amount + receipt.totalFee() })
      .verify()
  })

  describe('native transfer events', () => {
    it('account to account transfer should emit an event', async () => {
      const { sender, receiver } = await clients()
      const amount = parseEther('0.0000001')

      const receipt = await sender
        .sendTransaction({ to: receiver.account.address, value: amount })
        .then(ReceiptVerifier.waitSuccess)

      // Zero5: base 21000 + caller check 2100 + recipient check 2100 = 25200
      receipt.verifyGasUsed(25200n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: receiver, amount })
      })
    })

    it('account to account zero transfer should not emit an event', async () => {
      const { sender, receiver } = await clients()

      const receipt = await sender
        .sendTransaction({ to: receiver.account.address, value: 0n })
        .then(ReceiptVerifier.waitSuccess)

      // Zero5: base 21000 + caller check 2100 = 23100 (no recipient check for zero value)
      receipt.verifyGasUsed(23100n).verifyNoEvents()
    })

    it('calling and sending to a contract should emit an event', async () => {
      const { sender } = await clients()
      const amount = parseEther('0.0000001')

      const receipt = await nativeTransferHelper.callCanReceive(sender, amount).then(ReceiptVerifier.build)

      // Zero5: base ~21160 + caller check 2100 + recipient check 2100 = ~25360
      receipt.verifyGasUsedApproximately(25360n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount: amount })
      })
    })

    it('chained calls should emit multiple events', async () => {
      const { sender } = await clients()
      const callerAmount = parseEther('0.0000001')
      const relayAmount = parseEther('0.00000005')

      const receipt = await nativeTransferHelper
        .callRelay(
          sender,
          altNativeTransferHelper.address,
          callerAmount,
          relayAmount,
          true,
          nativeTransferHelper.encodeCanReceiveCalldata(),
        )
        .then(ReceiptVerifier.build)

      // Zero5: base + caller/recipient blocklist checks + nested frame warm/cold SLOAD costs
      receipt.verifyGasUsedApproximately(38786n).verifyEvents((ev) => {
        ev.expectCount(2)
          .expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount: callerAmount })
          .expectNativeTransfer({
            from: nativeTransferHelper.address,
            to: altNativeTransferHelper.address,
            amount: relayAmount,
          })
      })
    })

    // EOA --> A --> B (revert, due to calling non-payable function)
    // EOA calls A with a value
    // A calls B with a value
    // B reverts due to receiving value but non-payable fallback
    // A ignores the CALL result and continues
    it('nested calls with a reverted inner frame do not emit events', async () => {
      const { client, sender } = await clients()
      const callerAmount = parseEther('0.0000001')
      const relayAmount = parseEther('0.00000005')

      const balances = await balancesSnapshot(client, {
        nativeTransferHelper: nativeTransferHelper.address,
        altNativeTransferHelper: altNativeTransferHelper.address,
        sender,
      })

      // The second call will revert, but we do not require success, so the overall tx will succeed
      const receipt = await nativeTransferHelper
        .callRelay(
          sender,
          altNativeTransferHelper.address,
          callerAmount,
          relayAmount,
          false, // ignore the inner relay result
          nativeTransferHelper.encodeCannotReceiveCalldata(),
        )
        .then(ReceiptVerifier.build)

      // Zero5: base + caller/recipient blocklist checks + nested frame warm/cold SLOAD costs
      receipt.verifyGasUsedApproximately(38806n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount: callerAmount })
      })

      await balances
        .increase({
          nativeTransferHelper: callerAmount,
        })
        .decrease({
          sender: callerAmount + receipt.totalFee(),
        })
        .verify()
    })

    // EOA --> A --> B
    // EOA calls A with a value
    // A calls B with a value
    // The call from A --> B fails before reaching B due to pre-frame checks
    it('CALLs that fail pre-frame execution due to insufficient balance do not emit events', async () => {
      const { client, sender } = await clients()
      const callerAmount = parseEther('0.0000001')
      const relayAmount = parseEther('10000') // excessive amount to trigger pre-frame balance check failure

      // Sanity check that insufficient balance will indeed occur
      expect(await client.getBalance({ address: nativeTransferHelper.address })).to.be.lt(relayAmount + callerAmount)

      const balances = await balancesSnapshot(client, {
        nativeTransferHelper: nativeTransferHelper.address,
        altNativeTransferHelper: altNativeTransferHelper.address,
        sender,
      })

      const receipt = await nativeTransferHelper
        .callRelay(
          sender,
          altNativeTransferHelper.address,
          callerAmount,
          relayAmount,
          false, // ignore the inner relay result
          nativeTransferHelper.encodeCanReceiveCalldata(), // this would succeed if balances were sufficient
        )
        .then(ReceiptVerifier.build)

      // Zero5: base + caller/recipient blocklist checks + nested frame warm/cold SLOAD costs
      receipt.verifyGasUsedApproximately(38712n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount: callerAmount })
      })

      await balances
        .increase({
          nativeTransferHelper: callerAmount,
        })
        .decrease({
          sender: callerAmount + receipt.totalFee(),
        })
        .verify()
    })

    // EOA --> A
    // EOA calls A with a value
    // A performs CREATE passing an excessive callvalue
    it('CREATE that fails with insufficient balance does not emit events', async () => {
      const { client, sender } = await clients()

      const nativeTransferHelperbalance = await client.getBalance({ address: nativeTransferHelper.address })

      const balances = await balancesSnapshot(client, {
        nativeTransferHelper: nativeTransferHelper.address,
        altNativeTransferHelper: altNativeTransferHelper.address,
        sender,
      })

      const receipt = await nativeTransferHelper
        .callCreate(
          sender,
          '0x00',
          1n, // value sent to create()
          nativeTransferHelperbalance + 2n, // at least one more than the helper's balance
        )
        .then(ReceiptVerifier.build)

      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount: 1n })
      })

      await balances
        .increase({
          nativeTransferHelper: 1n,
        })
        .decrease({
          sender: 1n + receipt.totalFee(),
        })
        .verify()
    })

    it('CREATE from an EOA should emit an event if value is transferred', async () => {
      const { client, sender } = await clients()
      const amount = parseEther('0.0000001')

      const receipt = await NativeTransferHelper.deploy(sender, client, amount)
        .then((h) => h.deploymentReceipt)
        .then(ReceiptVerifier.build)

      expect(receipt.contractAddress).to.not.be.null
      receipt.verifyGasUsedApproximately(297425n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: receipt.contractAddress ?? '0x0', amount })
      })
    })

    it('CREATE from an EOA should not emit an event if no value is transferred', async () => {
      const { client, sender } = await clients()

      const receipt = await NativeTransferHelper.deploy(sender, client, 0n)
        .then((h) => h.deploymentReceipt)
        .then(ReceiptVerifier.build)

      expect(receipt.contractAddress).to.not.be.null
      receipt.verifyGasUsedApproximately(297425n).verifyNoEvents()
    })

    it('CREATE from a contract should emit an event if value is transferred', async () => {
      const { client, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Predict CREATE address
      const nonceBefore = await client.getTransactionCount({ address: nativeTransferHelper.address })
      const expectedDeployedAddr = getCreateAddress({ from: nativeTransferHelper.address, nonce: BigInt(nonceBefore) })

      // Call and CREATE
      const receipt = await nativeTransferHelper
        .callCreate(sender, callHelperArtifact.bytecode, amount, amount)
        .then(ReceiptVerifier.build)

      receipt.verifyGasUsedApproximately(864185n).verifyEvents((ev) => {
        ev.expectCount(2)
          .expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount })
          .expectNativeTransfer({ from: nativeTransferHelper.address, to: expectedDeployedAddr, amount })
      })

      expect(await client.getCode({ address: expectedDeployedAddr })).to.equal(callHelperArtifact.deployedBytecode)
    })

    it('CREATE from a contract should not emit an event if no value is transferred', async () => {
      const { sender } = await clients()

      // Call and CREATE
      const receipt = await nativeTransferHelper
        .callCreate(sender, nativeTransferHelper.bytecode, 0n, 0n)
        .then(ReceiptVerifier.build)

      receipt.verifyGasUsedApproximately(76601n).verifyNoEvents()
    })

    it('CREATE2 should emit an event if value is transferred', async () => {
      const { sender } = await clients()
      const amount = parseEther('0.0000001')
      const salt = keccak256('0xdeadBeef123')

      // Predict CREATE2 address
      const deploymentBytecode = nativeTransferHelper.encodeDeploymentBytecode()
      const expectedDeployedAddr = getCreate2Address({
        bytecode: deploymentBytecode,
        from: nativeTransferHelper.address,
        salt,
      })

      // Call and CREATE2
      const receipt = await nativeTransferHelper
        .callCreate2(sender, deploymentBytecode, salt, amount)
        .then(ReceiptVerifier.build)

      // Zero5: base + caller/recipient blocklist checks + nested frame warm/cold SLOAD costs
      receipt.verifyGasUsedApproximately(83987n).verifyEvents((ev) => {
        ev.expectCount(2)
          .expectNativeTransfer({ from: sender, to: nativeTransferHelper.address, amount })
          .expectNativeTransfer({ from: nativeTransferHelper.address, to: expectedDeployedAddr, amount })
      })
    })

    it('CREATE2 should not emit an event if no value is transferred', async () => {
      const { sender } = await clients()
      const salt = keccak256('0xdeadBeef456')

      // Call and CREATE2
      const receipt = await nativeTransferHelper
        .callCreate2(sender, nativeTransferHelper.encodeDeploymentBytecode(), salt, 0n)
        .then(ReceiptVerifier.build)

      // Zero5: base + caller check 2100 + nested frame SLOAD costs
      receipt.verifyGasUsedApproximately(79799n).verifyNoEvents()
    })

    it('SELFDESTRUCT from deployment transaction should NOT emit an event for zero amounts', async () => {
      const { sender, receiver, client } = await clients()

      // Deploy and SELFDESTRUCT, sending to receiver
      const txn = await NativeTransferHelper.deploy(sender, client, 0n, receiver.account.address)
        .then((h) => h.deploymentReceipt)
        .then(ReceiptVerifier.build)

      txn.verifyGasUsedApproximately(79849n).verifyNoEvents()
    })

    it('SELFDESTRUCT from deployment transaction should emit an event for non-zero amounts', async () => {
      const { sender, receiver, client } = await clients()
      const amount = parseEther('0.0000001')

      // Calculated expected contract address
      const nonceBefore = await client.getTransactionCount({ address: sender.account.address })
      const expectedDeployedAddr = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonceBefore) })

      // Deploy and SELFDESTRUCT, sending to receiver
      const txn = await NativeTransferHelper.deploy(sender, client, amount, receiver.account.address)
        .then((h) => h.deploymentReceipt)
        .then(ReceiptVerifier.build)

      // Zero5: base ~79849 + caller check 2100 + recipient check 2100 = ~84049
      txn.verifyGasUsedApproximately(84049n).verifyEvents((ev) => {
        ev.expectCount(2)
          .expectNativeTransfer({ from: sender, to: expectedDeployedAddr, amount })
          .expectNativeTransfer({ from: expectedDeployedAddr, to: receiver.account.address, amount })
      })
    })

    it('SELFDESTRUCT should NOT emit an event for zero amounts', async () => {
      const { sender, receiver, client } = await clients()

      // Deploy helper contract
      const helper = await NativeTransferHelper.deploy(sender, client, 0n)

      // Call SELFDESTRUCT
      const receipt = await helper.callSelfDestruct(sender, receiver.account.address).then(ReceiptVerifier.build)

      // Zero5: base ~29380 + caller check 2100 = ~31480 (no recipient check for zero value)
      receipt.verifyGasUsedApproximately(31480n).verifyNoEvents()
    })

    it('SELFDESTRUCT should emit an event for non-zero amounts', async () => {
      const { sender, receiver, client } = await clients()
      const amount = parseEther('0.0000001')

      // Calculated expected contract address
      const nonceBefore = await client.getTransactionCount({ address: sender.account.address })
      const expectedDeployedAddr = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonceBefore) })

      // Deploy and then SELFDESTRUCT, sending to receiver
      const helper = await NativeTransferHelper.deploy(sender, client, amount)

      // SELFDESTRUCT
      const txn = await helper.callSelfDestruct(sender, receiver.account.address).then(ReceiptVerifier.build)

      // Zero5: base ~29380 + caller check 2100 = ~31480
      txn.verifyGasUsedApproximately(31480n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: expectedDeployedAddr, to: receiver.account.address, amount })
      })
    })
  })

  describe('selfdestruct', () => {
    it('SELFDESTRUCT succeeds during deployment txns', async () => {
      const { sender, receiver, client } = await clients()
      const amount = parseEther('0.0000001')

      // Calculated expected contract address
      const nonceBefore = await client.getTransactionCount({ address: sender.account.address })
      const expectedDeployedAddr = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonceBefore) })

      // Deploy and SELFDESTRUCT, sending to receiver
      await NativeTransferHelper.deploy(sender, client, amount, receiver.account.address)

      // Check that the contract is destroyed
      expect(await client.getBalance({ address: expectedDeployedAddr })).to.equal(0n)
      expect(await client.getCode({ address: expectedDeployedAddr })).to.be.undefined
    })

    it('SELFDESTRUCT reverts for non-zero transfers to self during deployment txns', async () => {
      const { sender, client } = await clients()
      const amount = parseEther('0.0000001')

      // Calculated expected contract address
      const nonceBefore = await client.getTransactionCount({ address: sender.account.address })
      const expectedDeployedAddr = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonceBefore) })

      // Deploy and SELFDESTRUCT, setting the target as itself, aka a burn in this case
      await expect(NativeTransferHelper.deploy(sender, client, amount, expectedDeployedAddr)).to.be.rejected
    })

    it('SELFDESTRUCT succeeds outside of deployment txns', async () => {
      const { sender, receiver, client } = await clients()
      const amount = parseEther('0.0000001')

      // Deploy helper contract
      const helper = await NativeTransferHelper.deploy(sender, client, amount)

      // Check state before
      const codeBefore = await client.getCode({ address: helper.address })
      const receiverBalanceBefore = await client.getBalance({ address: receiver.account.address })

      // Call SELFDESTRUCT
      await helper.callSelfDestruct(sender, receiver.account.address)

      // Check that the balance moved, but the contract was not destroyed
      expect(await client.getBalance({ address: helper.address })).to.equal(0n)
      expect(await client.getBalance({ address: receiver.account.address })).to.equal(receiverBalanceBefore + amount)
      expect(await client.getCode({ address: helper.address })).to.equal(codeBefore)
    })

    it('SELFDESTRUCT reverts for any non-zero transfers to self', async () => {
      const { sender, client } = await clients()
      const amount = parseEther('0.0000001')

      // Deploy helper contract
      const helper = await NativeTransferHelper.deploy(sender, client, amount)

      // Call SELFDESTRUCT
      await expect(helper.callSelfDestruct(sender, helper.address)).to.be.rejected

      // Balance unchanged
      expect(await client.getBalance({ address: helper.address })).to.equal(amount)
    })
  })
})
