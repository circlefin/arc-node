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

import { expect } from 'chai'
import { balancesSnapshot, NativeCoinAuthority, NativeTransferHelper, ReceiptVerifier, getClients } from '../helpers'
import { ProtocolConfig } from '../helpers/ProtocolConfig'
import { signPermit, USDC } from '../helpers/FiatToken'
import { NativeCoinControl, ERR_BLOCKED_ADDRESS } from '../helpers/NativeCoinControl'
import {
  toHex,
  maxUint256,
  parseEther,
  getCreateAddress,
  getCreate2Address,
  keccak256,
  encodeFunctionData,
  parseAbi,
  Address,
  zeroAddress,
  TransactionExecutionError,
  ContractFunctionExecutionError,
} from 'viem'
import { CallHelper } from '../helpers/CallHelper'
import { toBytes32 } from '../../scripts/genesis/types'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'

describe('NativeFiatToken', () => {
  let nativeTransferHelperA: NativeTransferHelper
  let nativeTransferHelperB: NativeTransferHelper
  let nativeTransferHelperC: NativeTransferHelper

  const clients = async () => {
    const { client, ...rest } = await getClients()
    const usdc = USDC.attach(client).read
    const totalSupply = async () => NativeCoinAuthority.totalSupply(client)
    return { ...rest, client, totalSupply, usdc }
  }

  before(async () => {
    const { client, admin } = await clients()
    // Deploy helper contract for testing internal native transfers
    nativeTransferHelperA = await NativeTransferHelper.deploy(admin, client, 0n)
    nativeTransferHelperB = await NativeTransferHelper.deploy(admin, client, 0n)
    nativeTransferHelperC = await NativeTransferHelper.deploy(admin, client, 0n)

    // Configure nativeTransferHelperA as a USDC burner
    await USDC.attach(admin)
      .write.configureMinter([nativeTransferHelperA.address, 0n])
      .then(ReceiptVerifier.waitSuccess)
  })

  it('transfer with non-zero amount', async () => {
    const { client, usdc, sender, receiver, totalSupply } = await clients()
    const amount = USDC.parseUnits('0.002931')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      totalSupply,
      senderUSDC: () => usdc.balanceOf([sender.account.address]),
      receiverUSDC: () => usdc.balanceOf([receiver.account.address]),
      totalSupplyUSDC: () => usdc.totalSupply(),
    })

    const receipt = await USDC.attach(sender)
      .write.transfer([receiver.account.address, amount])
      .then(ReceiptVerifier.waitSuccess)
    // Zero5: EIP-2929 warm/cold gas pricing
    receipt.verifyGasUsedApproximately(54638n).verifyEvents((ev) => {
      ev.expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amount) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amount })
        .expectAllEventsMatched()
    })

    await balances
      .increase({
        receiver: USDC.toNative(amount),
        receiverUSDC: amount,
      })
      .decrease({
        sender: USDC.toNative(amount) + receipt.totalFee(),
        senderUSDC: amount + USDC.fromNative(receipt.totalFee()).notAccurate,
      })
      .verifyWithOverride({
        senderUSDC: (_, after) => USDC.fromNative(after.sender).roundDown,
      })
  })

  it('transfer with zero amount', async () => {
    const { client, usdc, sender, receiver, totalSupply } = await clients()
    const amount = USDC.parseUnits('0')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      totalSupply,
      senderUSDC: () => usdc.balanceOf([sender.account.address]),
      receiverUSDC: () => usdc.balanceOf([receiver.account.address]),
      totalSupplyUSDC: () => usdc.totalSupply(),
    })

    const receipt = await USDC.attach(sender)
      .write.transfer([receiver.account.address, amount])
      .then(ReceiptVerifier.waitSuccess)
    // Zero5: EIP-2929 warm/cold gas pricing
    receipt.verifyGasUsedApproximately(40713n).verifyEvents((ev) => {
      ev.expectUSDCTransfer({ from: sender, to: receiver, value: amount }).expectAllEventsMatched()
    })

    await balances
      .decrease({
        sender: receipt.totalFee(),
        senderUSDC: USDC.fromNative(receipt.totalFee()).notAccurate,
      })
      .verifyWithOverride({
        senderUSDC: (_, after) => USDC.fromNative(after.sender).roundDown,
      })
  })

  it('transfer with non-zero amount oog', async () => {
    const { sender, receiver } = await clients()
    const receipt = await USDC.attach(sender)
      .write.transfer([receiver.account.address, 1n], { gas: 40000n })
      .then(ReceiptVerifier.wait)
    receipt.isReverted().verifyGasUsedApproximately(39142n)
  })

  it('transfer with zero amount oog', async () => {
    const { sender, receiver } = await clients()
    const receipt = await USDC.attach(sender)
      .write.transfer([receiver.account.address, 0n], { gas: 32100n })
      .then(ReceiptVerifier.wait)
    receipt.isReverted().verifyGasUsedApproximately(32100n)
  })

  it('self transfer', async () => {
    const { sender, client, usdc } = await clients()
    const balances = await balancesSnapshot(client, {
      sender,
      totalSupplyUSDC: () => usdc.totalSupply(),
    })

    const receipt = await sender
      .sendTransaction({ to: sender.account.address, value: 127n })
      .then(ReceiptVerifier.waitSuccess)
    // EIP-7708: self-transfers (from == to) emit no Transfer log
    receipt.verifyNoEvents()
    await balances.decrease({ sender: receipt.totalFee() }).verify()
  })

  it('mint', async () => {
    const { client, usdc, operator, receiver, totalSupply } = await clients()
    const amount = USDC.parseUnits('0.002931')
    const balances = await balancesSnapshot(client, {
      operator,
      receiver,
      totalSupply,
      receiverUSDC: () => usdc.balanceOf([receiver.account.address]),
      totalSupplyUSDC: () => usdc.totalSupply(),
    })

    const receipt = await USDC.attach(operator)
      .write.mint([receiver.account.address, amount])
      .then(ReceiptVerifier.waitSuccess)
    receipt.verifyEvents((ev) => {
      ev.expectNativeMint({ recipient: receiver, amount: USDC.toNative(amount) })
        .expectUSDCMint({ minter: operator, to: receiver, amount: amount })
        .expectUSDCTransfer({ from: zeroAddress, to: receiver, value: amount })
        .expectAllEventsMatched()
    })

    await balances
      .increase({
        receiver: USDC.toNative(amount),
        receiverUSDC: amount,
        totalSupply: USDC.toNative(amount),
        totalSupplyUSDC: amount,
      })
      .decrease({
        operator: receipt.totalFee(),
      })
      .verify()
  })

  it('burn', async () => {
    const { client, usdc, operator, totalSupply } = await clients()
    const amount = USDC.parseUnits('0.000001')
    const balances = await balancesSnapshot(client, {
      operator,
      totalSupply,
      operatorUSDC: () => usdc.balanceOf([operator.account.address]),
      totalSupplyUSDC: () => usdc.totalSupply(),
    })

    const receipt = await USDC.attach(operator).write.burn([amount]).then(ReceiptVerifier.waitSuccess)
    receipt.verifyEvents((ev) => {
      ev.expectNativeBurn({ from: operator, amount: USDC.toNative(amount) })
        .expectUSDCBurn({ burner: operator, amount: amount })
        .expectUSDCTransfer({ from: operator, to: zeroAddress, value: amount })
        .expectAllEventsMatched()
    })

    await balances
      .decrease({
        operator: USDC.toNative(amount) + receipt.totalFee(),
        totalSupply: USDC.toNative(amount),
        operatorUSDC: amount + USDC.fromNative(receipt.totalFee()).notAccurate,
        totalSupplyUSDC: amount,
      })
      .verifyWithOverride({
        operatorUSDC: (_, after) => USDC.fromNative(after.operator).roundDown,
      })
  })

  describe('native coin control', () => {
    it('blocklist operations: operator blocklist and unblocklist targetAccount', async () => {
      const { client, operator, createRandWallet } = await clients()
      const targetAccount = await createRandWallet().then((x) => x.account.address)

      // Verify targetAccount is not initially blocklisted
      const initialBlocklistStatus = await NativeCoinControl.isBlocklisted(client, targetAccount)
      expect(initialBlocklistStatus).to.be.false

      // Blocklist the targetAccount address
      const blocklistReceipt = await USDC.attach(operator)
        .write.blacklist([targetAccount])
        .then(ReceiptVerifier.waitSuccess)

      // Verify blocklist event was emitted
      // Zero5: EIP-2929 warm/cold gas pricing
      // +3526 gas for owner blocklist protection check
      blocklistReceipt.verifyGasUsedApproximately(61574n).verifyEvents((ev) => {
        ev.expectNativeBlocklisted({ account: targetAccount })
          .expectUSDCBlacklisted({ account: targetAccount })
          .expectAllEventsMatched()
      })

      // Verify targetAccount is now blocklisted
      const blocklistedStatus = await NativeCoinControl.isBlocklisted(client, targetAccount)
      expect(blocklistedStatus).to.be.true

      // Unblocklist the targetAccount address
      const unblocklistReceipt = await USDC.attach(operator)
        .write.unBlacklist([targetAccount])
        .then(ReceiptVerifier.waitSuccess)

      // Verify unblocklist event was emitted
      // Zero5: cold SSTORE non-zero→zero (EIP-2200)
      unblocklistReceipt.verifyGasUsedApproximately(40862n).verifyEvents((ev) => {
        ev.expectNativeUnBlocklisted({ account: targetAccount })
          .expectUSDCUnBlacklisted({ account: targetAccount })
          .expectAllEventsMatched()
      })

      // Verify targetAccount is no longer blocklisted
      const finalBlocklistStatus = await NativeCoinControl.isBlocklisted(client, targetAccount)
      expect(finalBlocklistStatus).to.be.false
    })

    it('mempool blocklist: blocklisted random account cannot transfer native coins', async () => {
      const { client, operator, receiver, createRandWallet } = await clients()
      const amount = parseEther('0.0000001')
      const sender = await createRandWallet(parseEther('0.1'))

      // Verify random wallet can transfer before blocklist
      const testTransferTx = await sender.sendTransaction({
        to: receiver.account.address,
        value: amount,
      })
      await client.waitForTransactionReceipt({ hash: testTransferTx })

      // Blocklist the random wallet address
      const blocklistReceipt = await USDC.attach(operator)
        .write.blacklist([sender.account.address])
        .then(ReceiptVerifier.waitSuccess)

      // Verify blocklist event was emitted
      // Zero5: EIP-2929 warm/cold gas pricing
      // +3526 gas for owner blocklist protection check
      blocklistReceipt.verifyGasUsedApproximately(61574n).verifyEvents((ev) => {
        ev.expectNativeBlocklisted({ account: sender })
          .expectUSDCBlacklisted({ account: sender })
          .expectAllEventsMatched()
      })

      // Setup balance tracking AFTER blocklist operation
      const balances = await balancesSnapshot(client, {
        randomWallet: sender.account.address,
        receiver: receiver.account.address,
      })

      // Verify random wallet is now blocklisted by attempting a native transfer (should fail)
      // high gas to ensure the transaction is included in the mempool
      await expect(
        sender.sendTransaction({ to: receiver.account.address, value: amount, gas: 600000n }),
      ).to.be.rejectedWith(TransactionExecutionError, ERR_BLOCKED_ADDRESS)

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator)
        .write.unBlacklist([sender.account.address])
        .then((hash: `0x${string}`) => client.waitForTransactionReceipt({ hash }))
    })

    it('pre-execution blocklist: blocklisted sender cannot transfer native coins', async () => {
      const { client, operator, createRandWallet, receiver } = await clients()
      const amount = parseEther('0.0000001')
      const sender = await createRandWallet(parseEther('0.1'))

      // Blocklist the sender address
      const blocklistReceipt = await USDC.attach(operator)
        .write.blacklist([sender.account.address])
        .then(ReceiptVerifier.waitSuccess)

      // Verify blocklist event was emitted
      // Zero5: EIP-2929 warm/cold gas pricing
      // +3526 gas for owner blocklist protection check
      blocklistReceipt.verifyGasUsedApproximately(61562n).verifyEvents((ev) => {
        ev.expectNativeBlocklisted({ account: sender })
          .expectUSDCBlacklisted({ account: sender })
          .expectAllEventsMatched()
      })

      // Setup balance tracking AFTER blocklist operation
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        receiver: receiver.account.address,
      })

      // Verify sender is now blocklisted by attempting a native transfer (should fail)
      await expect(sender.sendTransaction({ to: receiver.account.address, value: amount })).to.be.rejectedWith(
        TransactionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([sender.account.address]).then(ReceiptVerifier.waitSuccess)
    })

    it('pre-execution blocklist: blocklisted sender cannot make zero-value calls', async () => {
      const { client, operator, createRandWallet, receiver } = await clients()
      const sender = await createRandWallet(parseEther('0.1'))

      // Blocklist the sender address
      await USDC.attach(operator).write.blacklist([sender.account.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking AFTER blocklist operation
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        receiver: receiver.account.address,
      })

      // Verify blocklisted sender cannot make even zero-value calls
      await expect(sender.sendTransaction({ to: receiver.account.address, value: 0n })).to.be.rejectedWith(
        TransactionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([sender.account.address]).then(ReceiptVerifier.waitSuccess)
    })

    it('pre-execution blocklist: blocklisted sender cannot make USDC transfers', async () => {
      const { client, operator, createRandWallet, receiver } = await clients()
      const amount = USDC.parseUnits('0.001')
      const sender = await createRandWallet(parseEther('0.1'))

      // Blocklist the sender address
      await USDC.attach(operator).write.blacklist([sender.account.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking AFTER blocklist operation
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        receiver: receiver.account.address,
      })

      // Verify blocklisted sender cannot make USDC transfers
      await expect(USDC.attach(sender).write.transfer([receiver.account.address, amount])).to.be.rejectedWith(
        ContractFunctionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([sender.account.address]).then(ReceiptVerifier.waitSuccess)
    })

    it('pre-execution blocklist: cannot transfer native coins to blocklisted recipient', async () => {
      const { client, operator, sender, createRandWallet } = await clients()
      const amount = parseEther('0.0000001')
      const receiver = await createRandWallet()

      // Blocklist the receiver address
      await USDC.attach(operator).write.blacklist([receiver.account.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking AFTER blocklist operation
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        receiver: receiver.account.address,
      })

      // Verify transfer TO blocklisted recipient fails
      await expect(sender.sendTransaction({ to: receiver.account.address, value: amount })).to.be.rejectedWith(
        ERR_BLOCKED_ADDRESS,
      )

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([receiver.account.address]).then(ReceiptVerifier.waitSuccess)
    })

    it('pre-execution blocklist: can make zero-value calls to blocklisted recipient', async () => {
      const { client, operator, sender, createRandWallet } = await clients()
      const receiver = await createRandWallet().then((x) => x.account)

      // Get beneficiary address from ProtocolConfig
      const protocolConfig = ProtocolConfig.attach(client)
      const beneficiary = await protocolConfig.read.rewardBeneficiary()

      // Blocklist the receiver address first
      await USDC.attach(operator).write.blacklist([receiver.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        beneficiary,
        sender: sender.account.address,
        receiver: receiver.address,
      })

      // Verify zero-value call TO blocklisted recipient succeeds
      const receiptVerifier = await sender
        .sendTransaction({ to: receiver.address, value: 0n })
        .then(ReceiptVerifier.waitSuccess)

      // Calculate gas fees and verify balance changes
      const totalFee = receiptVerifier.totalFee()

      // Verify that gas was consumed from sender and reward sent to beneficiary
      await balances
        .increase({
          beneficiary: totalFee, // Beneficiary receives gas fees
        })
        .decrease({
          sender: totalFee, // Sender pays the gas fees
        })
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([receiver.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 1: First frame recipient is blocklisted with value transfer
    //
    // Flow: EOA -> A{value>0} -> B[unreachable]
    // Blocklist: A is blocklisted
    // Value: EOA->A has value
    //
    // Frame execution:
    //   EOA -> A init: REVERT (A blocklisted, value transfer)
    //
    // Result: Transaction reverted at first frame
    it('execution frames blocklist: first frame fails when recipient is blocklisted with value', async () => {
      const { client, operator, sender } = await clients()
      const callerAmount = parseEther('0.0000001')
      const relayAmount = parseEther('0.00000005')

      // Blocklist the nativeTransferHelperA address
      await USDC.attach(operator).write.blacklist([nativeTransferHelperA.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
      })

      // This should fail due to execution frame blocklist check when trying to transfer to blocklisted recipient
      await expect(
        nativeTransferHelperA.callRelay(
          sender,
          nativeTransferHelperB.address,
          callerAmount,
          relayAmount,
          true,
          nativeTransferHelperA.encodeCanReceiveCalldata(),
        ),
      ).to.be.rejectedWith(ContractFunctionExecutionError, ERR_BLOCKED_ADDRESS)

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([nativeTransferHelperA.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 2: Inner frame recipient is blocklisted, error handled gracefully
    //
    // Flow: EOA -> A{value>0} -> B{value>0} -> [unreachable]
    // Blocklist: B is blocklisted
    // Value: EOA->A and A->B both have value
    // Error handling: requireSuccess=false
    //
    // Frame execution:
    //   EOA -> A init: SUCCESS
    //   EOA -> A run: SUCCESS
    //      A -> B init: REVERT (B blocklisted, value transfer)
    //   EOA -> A run: CONTINUE (requireSuccess=false)
    //
    // Result: Transaction succeeds, A handles B's failure gracefully
    it('execution frames blocklist: inner frame fails but transaction succeeds with requireSuccess false', async () => {
      const { client, operator, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Blocklist the nativeTransferHelperB address
      await USDC.attach(operator).write.blacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
        nativeTransferHelperC: nativeTransferHelperC.address,
      })

      // Create the call chain: A -> B -> C
      // First, encode the call from B to C (innermost call)
      const callBToC = nativeTransferHelperB.encodeRelayCalldata(
        nativeTransferHelperC.address,
        amount,
        true,
        nativeTransferHelperC.encodeCanReceiveCalldata(),
      )

      // Execute the triple chain: sender -> A -> B -> C
      const receipt = await nativeTransferHelperA
        .callRelay(sender, nativeTransferHelperB.address, amount, amount, false, callBToC)
        .then(ReceiptVerifier.build)

      // Verify we have 1 transfer events for the chain
      // - Event 0: sender -> nativeTransferHelperA
      receipt.verifyGasUsedApproximately(37760n).verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelperA.address, amount })
      })

      // Verify balance changes
      await balances
        .increase({
          nativeTransferHelperA: amount, // A receives the amount
        })
        .decrease({
          sender: amount + receipt.totalFee(), // Sender pays amount + gas fees
        })
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 3: Inner frame recipient is blocklisted, error propagated
    //
    // Flow: EOA -> A{value>0} -> B{value>0} -> [unreachable]
    // Blocklist: B is blocklisted
    // Value: EOA->A and A->B both have value
    // Error handling: requireSuccess=true
    //
    // Frame execution:
    //   EOA -> A init: SUCCESS
    //   EOA -> A run: SUCCESS
    //     A -> B init: REVERT (B blocklisted, value transfer)
    //   EOA -> A run: REVERT (requireSuccess=true)
    //
    // Result: Transaction reverted, A propagates B's failure
    it('execution frames blocklist: inner frame fails and transaction reverts with requireSuccess true', async () => {
      const { client, operator, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Blocklist the nativeTransferHelperB address
      await USDC.attach(operator).write.blacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
        nativeTransferHelperC: nativeTransferHelperC.address,
      })

      // Create the call chain: A -> B -> C
      // First, encode the call from B to C (innermost call)
      const callBToC = nativeTransferHelperB.encodeRelayCalldata(
        nativeTransferHelperC.address,
        amount,
        true,
        nativeTransferHelperC.encodeCanReceiveCalldata(),
      )

      // Execute the chain with requireSuccess = true: sender -> A -> B (should revert)
      await expect(
        nativeTransferHelperA.callRelay(
          sender,
          nativeTransferHelperB.address,
          amount,
          amount,
          true, // requireSuccess = true, so A will revert when B call fails
          callBToC,
        ),
        // FIXME no error message for address blocked?
      ).to.be.rejectedWith(ContractFunctionExecutionError, /Relay reverted/)

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Skip the estimation to send the transaction directly.
      const receipt = await nativeTransferHelperA
        .callRelay(
          sender,
          nativeTransferHelperB.address,
          amount,
          amount,
          true, // requireSuccess = true, so A will revert when B call fails
          callBToC,
          60000n, // set gas manually
        )
        .then(ReceiptVerifier.build)
      receipt.isReverted().verifyNoEvents().verifyGasUsedApproximately(37849n)

      // Verify that no balance changes occurred
      await balances.decrease({ sender: receipt.totalFee() }).verify()

      // Skip the estimation to send the transaction directly
      // Set a different gas limit to check the gas usage is consistant.
      const receipt2 = await nativeTransferHelperA
        .callRelay(
          sender,
          nativeTransferHelperB.address,
          amount,
          amount,
          true, // requireSuccess = true, so A will revert when B call fails
          callBToC,
          120000n, // set gas manually
        )
        .then(ReceiptVerifier.build)
      receipt2.isReverted().verifyNoEvents().verifyGasUsedApproximately(37849n)

      // Verify that no balance changes occurred
      await balances.decrease({ sender: receipt2.totalFee() }).verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 4: All frames use zero value transfers with blocklisted address
    //
    // Flow: EOA -> A{value=0} -> B{value=0} -> C{value=0}
    // Blocklist: B is blocklisted
    // Value: All transfers are zero value
    //
    // Frame execution:
    //   EOA -> A init: SUCCESS (zero value)
    //   EOA -> A run: SUCCESS
    //     A -> B init: SUCCESS (zero value, blocklist ignored)
    //     A -> B run: SUCCESS
    //       B -> C init: SUCCESS (zero value)
    //       B -> C run: SUCCESS
    //     A -> B run: SUCCESS
    //   EOA -> A run: SUCCESS
    //
    // Result: Transaction succeeds, zero-value calls bypass blocklist
    it('execution frames blocklist: all zero value calls succeed despite blocklisted addresses', async () => {
      const { client, operator, sender } = await clients()

      // Blocklist the nativeTransferHelperB address
      await USDC.attach(operator).write.blacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
        nativeTransferHelperC: nativeTransferHelperC.address,
      })

      // Create the call chain: A -> B -> C (all with zero value)
      // First, encode the call from B to C (innermost call)
      const callBToC = nativeTransferHelperB.encodeRelayCalldata(
        nativeTransferHelperC.address,
        0n, // Zero value
        true,
        nativeTransferHelperC.encodeCanReceiveCalldata(),
      )

      // Execute the triple chain with zero values: sender -> A -> B -> C
      const receipt = await nativeTransferHelperA
        .callRelay(
          sender,
          nativeTransferHelperB.address,
          0n, // Zero value
          0n, // Zero value
          true,
          callBToC,
        )
        .then(ReceiptVerifier.build)

      // Verify no transfer events since no value is transferred
      receipt.verifyNoEvents()

      // Verify balance changes - only gas fees are consumed
      await balances
        .decrease({
          sender: receipt.totalFee(), // Sender pays only gas fees
        })
        // All contract balances remain unchanged since no value was transferred
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 5: Mixed value transfers with blocklisted sender
    //
    // Flow: EOA -> A{value>0} -> B{value=0} -> C{value>0}
    // Blocklist: B is blocklisted
    // Value: EOA->A has value, A->B is zero, B->C has value
    //
    // Frame execution:
    //   EOA -> A init: SUCCESS (A not blocklisted)
    //   EOA -> A run: SUCCESS
    //     A -> B init: SUCCESS (zero value, blocklist ignored)
    //     A -> B run: SUCCESS
    //       B -> C init: REVERT (B blocklisted trying to send value)
    //     A -> B run: CONTINUE (requireSuccess=false)
    //   EOA -> A run: SUCCESS
    //
    // Result: Transaction succeeds, blocklisted address can receive zero-value calls but cannot send value
    it('execution frames blocklist: blocklisted address can receive zero value but cannot send value', async () => {
      const { client, operator, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Blocklist the nativeTransferHelperB address
      await USDC.attach(operator).write.blacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)

      // Setup balance tracking
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
        nativeTransferHelperC: nativeTransferHelperC.address,
      })

      // Create the call chain: A -> B -> C
      // First, encode the call from B to C with value (will fail due to B being blocklisted)
      const callBToC = nativeTransferHelperB.encodeRelayCalldata(
        nativeTransferHelperC.address,
        amount, // Value > 0, will fail because B is blocklisted
        false, // requireSuccess = false, so B won't revert when C call fails
        nativeTransferHelperC.encodeCanReceiveCalldata(),
      )

      // Execute the chain: sender -> A (value) -> B (zero) -> C (value, fails)
      const receipt = await nativeTransferHelperA
        .callRelay(
          sender,
          nativeTransferHelperB.address,
          amount, // Value to A
          0n, // Zero value to B
          false,
          callBToC,
        )
        .then(ReceiptVerifier.build)

      // Verify we have 1 transfer event (sender -> A only)
      // B -> C transfer fails because B is blocklisted when trying to send value
      // - Event 0: sender -> nativeTransferHelperA
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectNativeTransfer({ from: sender, to: nativeTransferHelperA.address, amount })
      })

      // Verify balance changes
      await balances
        .increase({
          nativeTransferHelperA: amount, // A receives the amount
        })
        .decrease({
          sender: amount + receipt.totalFee(), // Sender pays amount + gas fees
        })
        // B and C balances remain unchanged
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([nativeTransferHelperB.address]).then(ReceiptVerifier.waitSuccess)
    })

    // Case 6: Happy path - no blocklisted addresses
    //
    // Flow: EOA -> A{value>0} -> B{value>0} -> C{value>0}
    // Blocklist: No addresses are blocklisted
    // Value: All transfers have value
    //
    // Frame execution:
    //   EOA -> A init: SUCCESS
    //   EOA -> A run: SUCCESS
    //     A -> B init: SUCCESS
    //     A -> B run: SUCCESS
    //       B -> C init: SUCCESS
    //       B -> C run: SUCCESS
    //     A -> B run: SUCCESS
    //   EOA -> A run: SUCCESS
    //
    // Result: Transaction succeeds, all frames execute successfully with value transfers
    it('execution frames blocklist: all chained calls succeed when no addresses are blocklisted', async () => {
      const { client, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Setup balance tracking (no blocklist operations needed)
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelperA: nativeTransferHelperA.address,
        nativeTransferHelperB: nativeTransferHelperB.address,
        nativeTransferHelperC: nativeTransferHelperC.address,
      })

      // Create the call chain: A -> B -> C (all with value)
      // First, encode the call from B to C (innermost call)
      const callBToC = nativeTransferHelperB.encodeRelayCalldata(
        nativeTransferHelperC.address,
        amount, // Value > 0
        true,
        nativeTransferHelperC.encodeCanReceiveCalldata(),
      )

      const receipt = await nativeTransferHelperA
        .callRelay(
          sender,
          nativeTransferHelperB.address,
          amount, // Value to A
          amount, // Value to B
          true,
          callBToC,
        )
        .then(ReceiptVerifier.build)

      // Verify we have 3 transfer events for the complete chain
      // - Event 0: sender -> nativeTransferHelperA
      // - Event 1: nativeTransferHelperA -> nativeTransferHelperB
      // - Event 2: nativeTransferHelperB -> nativeTransferHelperC
      receipt.verifyEvents((ev) => {
        ev.expectCount(3)
          .expectNativeTransfer({ from: sender, to: nativeTransferHelperA.address, amount })
          .expectNativeTransfer({ from: nativeTransferHelperA.address, to: nativeTransferHelperB.address, amount })
          .expectNativeTransfer({ from: nativeTransferHelperB.address, to: nativeTransferHelperC.address, amount })
      })

      // Verify balance changes - final destination receives the amount
      await balances
        .increase({
          nativeTransferHelperC: amount, // Final destination receives the amount
        })
        .decrease({
          sender: amount + receipt.totalFee(), // Sender pays amount + gas fees
        })
        // A and B have no net balance change (receive and send same amount)
        .verify()
    })

    it('execution frames blocklist: CREATE from EOA fails when created address is blocklisted and value is transferred', async () => {
      const { client, operator, sender } = await clients()
      const amount = parseEther('0.0000001')

      // Calculate the address that will be created
      const nonce = await client.getTransactionCount({ address: sender.account.address })
      const predictedAddress = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonce) })

      // Blocklist the predicted contract address
      await USDC.attach(operator).write.blacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)

      // Verify the predicted address is blocklisted
      const isBlocklisted = await NativeCoinControl.isBlocklisted(client, predictedAddress)
      expect(isBlocklisted).to.be.true

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        predictedAddress,
      })

      // Attempt CREATE with value to blocklisted address - should fail due to execution frame blocklist
      await expect(NativeTransferHelper.deploy(sender, client, amount)).to.be.rejectedWith(
        TransactionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)
    })

    it('execution frames blocklist: CREATE from EOA succeeds when created address is blocklisted and no value is transferred', async () => {
      const { client, operator, sender } = await clients()

      // Calculate the address that will be created
      const nonce = await client.getTransactionCount({ address: sender.account.address })
      const predictedAddress = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonce) })

      // Blocklist the predicted contract address
      await USDC.attach(operator).write.blacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)
    })

    it('execution frames blocklist: CREATE from EOA succeeds when created address is blocklisted and no value is transferred', async () => {
      const { client, operator, sender } = await clients()

      // Calculate the address that will be created
      const nonce = await client.getTransactionCount({ address: sender.account.address })
      const predictedAddress = getCreateAddress({ from: sender.account.address, nonce: BigInt(nonce) })

      // Blocklist the predicted contract address
      await USDC.attach(operator).write.blacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)

      // Verify the predicted address is blocklisted
      const isBlocklisted = await NativeCoinControl.isBlocklisted(client, predictedAddress)
      expect(isBlocklisted).to.be.true

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        predictedAddress,
      })

      // Attempt CREATE with zero value to blocklisted address - should succeed since no value transfer
      const helper = await NativeTransferHelper.deploy(sender, client, 0n)
      const receipt = ReceiptVerifier.build(helper.deploymentReceipt)

      // Verify deployment was successful
      expect(receipt.contractAddress?.toLowerCase()).to.equal(predictedAddress.toLowerCase())
      expect(helper.deploymentReceipt.status).to.equal('success')

      // Verify no transfer events were emitted since no value was transferred
      receipt.verifyNoEvents()

      // Verify balance changes - sender should have paid gas, but no value transferred to contract
      await balances
        .decrease({
          sender: receipt.totalFee(), // Sender pays gas fees
        })
        // predictedAddress should remain at 0 since no value was transferred
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)
    })

    it('execution frames blocklist: CREATE2 from contract fails when created address is blocklisted and value is transferred', async () => {
      const { client, operator, sender } = await clients()
      const amount = parseEther('0.0000001')
      const salt = keccak256('0xdeadbeef123')

      // Predict CREATE2 address
      const deploymentBytecode = nativeTransferHelperA.encodeDeploymentBytecode()
      const predictedAddress = getCreate2Address({
        bytecode: deploymentBytecode,
        from: nativeTransferHelperA.address,
        salt,
      })

      // Blocklist the predicted contract address
      await USDC.attach(operator).write.blacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)

      // Verify the predicted address is blocklisted
      const isBlocklisted = await NativeCoinControl.isBlocklisted(client, predictedAddress)
      expect(isBlocklisted).to.be.true

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelper: nativeTransferHelperA.address,
        predictedAddress,
      })

      // Attempt CREATE2 with value to blocklisted address - should fail due to execution frame blocklist
      await expect(nativeTransferHelperA.callCreate2(sender, deploymentBytecode, salt, amount)).to.be.rejected

      // Verify that no balance changes occurred (no gas deduction for blocked transaction)
      await balances.verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)
    })

    it('execution frames blocklist: CREATE2 from contract succeeds when created address is blocklisted and no value is transferred', async () => {
      const { client, operator, sender } = await clients()
      const salt = keccak256('0xdeadbeef456')

      // Predict CREATE2 address
      const deploymentBytecode = nativeTransferHelperA.encodeDeploymentBytecode()
      const predictedAddress = getCreate2Address({
        bytecode: deploymentBytecode,
        from: nativeTransferHelperA.address,
        salt,
      })

      // Blocklist the predicted contract address
      await USDC.attach(operator).write.blacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)

      // Verify the predicted address is blocklisted
      const isBlocklisted = await NativeCoinControl.isBlocklisted(client, predictedAddress)
      expect(isBlocklisted).to.be.true

      // Setup balance tracking AFTER blocklist operation to avoid interference
      const balances = await balancesSnapshot(client, {
        sender: sender.account.address,
        nativeTransferHelper: nativeTransferHelperA.address,
        predictedAddress,
      })

      // Attempt CREATE2 with zero value to blocklisted address - should succeed since no value transfer
      const receipt = await nativeTransferHelperA
        .callCreate2(sender, deploymentBytecode, salt, 0n)
        .then(ReceiptVerifier.build)

      // Verify deployment was successful by checking no transfer events were emitted
      receipt.verifyNoEvents()

      // Verify balance changes - sender should have paid gas, but no value transferred to contracts
      await balances
        .decrease({
          sender: receipt.totalFee(), // Sender pays gas fees
        })
        // nativeTransferHelper and predictedAddress should remain unchanged since no value transfers
        .verify()

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([predictedAddress]).then(ReceiptVerifier.waitSuccess)
    })

    it('selfdestruct blocklist: cannot transfer native coins to blocklisted recipient', async () => {
      const { client, operator, sender, receiver } = await clients()
      const amount = parseEther('0.0000001')

      // Blocklist the receiver address
      await USDC.attach(operator).write.blacklist([receiver.account.address]).then(ReceiptVerifier.waitSuccess)

      // Deploy helper contract with some balance
      const helper = await NativeTransferHelper.deploy(sender, client, amount)

      // Attempt to selfdestruct to blocklisted receiver, expect revert with blocklist message
      await expect(helper.callSelfDestruct(sender, receiver.account.address)).to.be.rejectedWith(
        ContractFunctionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Balance unchanged
      expect(await client.getBalance({ address: helper.address })).to.equal(amount)

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([receiver.account.address]).then(ReceiptVerifier.waitSuccess)
    })

    it('selfdestruct blocklist: cannot selfdestruct when contract is blocklisted', async () => {
      const { client, operator, sender, receiver } = await clients()
      const amount = parseEther('0.0000001')

      // Deploy helper contract with some balance
      const helper = await NativeTransferHelper.deploy(sender, client, amount)

      // Blocklist the contract itself
      await USDC.attach(operator).write.blacklist([helper.address]).then(ReceiptVerifier.waitSuccess)

      // Attempt to selfdestruct, expect revert with blocklist message
      await expect(helper.callSelfDestruct(sender, receiver.account.address)).to.be.rejectedWith(
        ContractFunctionExecutionError,
        ERR_BLOCKED_ADDRESS,
      )

      // Balance unchanged
      expect(await client.getBalance({ address: helper.address })).to.equal(amount)

      // Unblocklist for cleanup
      await USDC.attach(operator).write.unBlacklist([helper.address]).then(ReceiptVerifier.waitSuccess)
    })

    describe('blocklisted A with different call types', () => {
      let blocklistedA: Address // CallHelper blocklisted
      let callHelperB: Address // CallHelper
      const blocklistedResult = CallHelper.encodeRevertMessage('Blocked address')
      const allCallTypes = ['execute', 'callCode', 'delegateCall', 'staticCall'] as const

      before(async () => {
        const { client, sender, operator } = await getClients()

        const A = await CallHelper.deterministicDeploy(sender, client, parseEther('1'), 0xdeadn)
        const B = await CallHelper.deterministicDeploy(sender, client, parseEther('1'), 10n)

        const usdc = USDC.attach(operator)
        if (!(await usdc.read.isBlacklisted([A.address]))) {
          await usdc.write.blacklist([A.address]).then(ReceiptVerifier.waitSuccess)
        }

        ;[blocklistedA, callHelperB] = [A.address, B.address]
      })

      for (const fn of allCallTypes) {
        // Since delegatecall and staticcall do not have value argument, it should always passed
        // skip here to speed up the test
        if (fn == 'delegateCall' || fn == 'staticCall') {
          continue
        }
        it(`Sender -> A.${fn} -> B`, async () => {
          const { sender, client } = await clients()
          const balances = await balancesSnapshot(client, { sender, blocklistedA, callHelperB })

          const receipt = await sender
            .sendTransaction({
              to: blocklistedA,
              data: CallHelper.encodeNested({ fn, target: callHelperB, value: 2n }),
              value: 0n,
            })
            .then(ReceiptVerifier.waitSuccess)

          receipt.verifyEvents((ev) => {
            if (fn == 'execute') {
              // call revert
              ev.expectExecutionResult({
                helper: blocklistedA,
                success: false,
                result: blocklistedResult,
              }).expectAllEventsMatched()
            } else {
              // delegateCall, staticcall, callCode successed, but do nothing
              ev.expectExecutionResult({ helper: blocklistedA, success: true, result: '0x' }).expectAllEventsMatched()
            }
          })
          await balances.decrease({ sender: receipt.totalFee() }).verify()
        })
      }

      for (const fn of allCallTypes) {
        it(`Sender -> A.${fn} -> B.transfer -> receiver`, async () => {
          const { sender, receiver, client } = await clients()
          const balances = await balancesSnapshot(client, { sender, blocklistedA, callHelperB, receiver })
          const amount1 = 5n
          const amount2 = 9n

          const receipt = await sender
            .sendTransaction({
              to: blocklistedA,
              data: CallHelper.encodeNested({
                fn,
                target: callHelperB,
                value: amount1,
                data: { fn: 'transfer', to: receiver.account.address, value: amount2 },
              }),
            })
            .then(ReceiptVerifier.waitSuccess)

          receipt.verifyEvents((ev) => {
            switch (fn) {
              case 'execute':
                // Sender -> A -X-> B -> receiver
                // call revert on the frame A -> B
                ev.expectExecutionResult({
                  helper: blocklistedA,
                  success: false,
                  result: blocklistedResult,
                }).expectAllEventsMatched()
                break
              case 'callCode':
                // Sender -> A.callCode -> B.transfer -> receiver
                // frame: Sender -> A.callCode
                //   {caller: Sender, target: A, value: 0, data: callCode...}
                //   frame: A.callCode -> B.transfer
                //     {caller: A, target: A, delegate: B, value: amount1, data: transfer...}
                //     frame: A.callCode -> B.transfer -> receiver
                //       {caller: A, target: receiver: value: amount2, data: 0x}
                //       revert: "address is blocklisted"
                //     event: ExecutionContext(A, amount1)
                //     event: ExecutionResult(false, "address is blocklisted")
                //   event: ExecutionResult(true, "0x")
                ev.expectExecutionContext({ helper: blocklistedA, sender: blocklistedA, value: amount1 })
                  .expectExecutionResult({ helper: blocklistedA, success: false, result: blocklistedResult })
                  .expectExecutionResult({ helper: blocklistedA, success: true, result: '0x' })
                  .expectAllEventsMatched()
                break
              case 'delegateCall':
                // Sender -> A.delegateCall -> B.transfer -> receiver
                // frame: Sender -> A.delegateCall
                //   {caller: Sender, target: A, value: 0, data: delegateCall...}
                //   frame: A.delegateCall -> B.transfer
                //     {caller: Sender, target: A, delegate: B, value: 0, data: transfer...}
                //     frame: A.delegateCall -> B.transfer -> receiver
                //       {caller: A, target: receiver: value: amount2, data: 0x}
                //       revert: "address is blocklisted"
                //     event: ExecutionContext(Sender, 0)
                //     event: ExecutionResult(false, "address is blocklisted")
                //   event: ExecutionResult(true, "0x")
                ev.expectExecutionContext({ helper: blocklistedA, sender, value: 0n })
                  .expectExecutionResult({ helper: blocklistedA, success: false, result: blocklistedResult })
                  .expectExecutionResult({ helper: blocklistedA, success: true, result: '0x' })
                  .expectAllEventsMatched()
                break
              case 'staticCall':
                // Sender -> A.staticCall -> B.transfer -> receiver
                // frame: Sender -> A.staticCall
                //   {caller: Sender, target: A, value: 0, data: staticCall...}
                //   frame: A.staticCall -> B.transfer
                //     {caller: Sender, target: A, isStatic: true, value: 0, data: transfer...}
                //     "error": "CallNotAllowedInsideStatic"
                //   event: ExecutionResult(false, "0x")
                ev.expectExecutionResult({
                  helper: blocklistedA,
                  success: false,
                  result: '0x', // EVM error, do not have error message
                }).expectAllEventsMatched()
                break
            }
          })
          await balances.decrease({ sender: receipt.totalFee() }).verify()
        })
      }
    })
  })

  it('draining an empty account will revert', async () => {
    const { client, sender, operator, createRandWallet } = await clients()
    const usdcAmount = USDC.parseUnits('10')

    // Create empty wallet (no balance, no nonce, no code)
    const emptyWallet = await createRandWallet(0n)

    // Mint 10 USDC to emptyWallet
    await USDC.attach(operator).write.mint([emptyWallet.account.address, usdcAmount]).then(ReceiptVerifier.waitSuccess)

    // Setup balance tracking and nonce is 0
    const balances = await balancesSnapshot(client, {
      emptyWalletNonce: () => client.getTransactionCount({ address: emptyWallet.account.address }).then(BigInt),
      emptyWallet: emptyWallet.account.address,
    })
    expect(balances.state().emptyWalletNonce).to.equal(0n)

    // Empty wallet signs an off-chain permit for SENDER to spend all 10 USDC
    // This is EIP-2612 permit
    // Sender will use this approval to transfer from empty wallet to themself
    const signature = await signPermit({
      client,
      wallet: emptyWallet,
      permitAmount: usdcAmount,
      spenderAddress: sender.account.address, // Sender needs approval to call transferFrom
    })

    // Sender submits the permit transaction on-chain (sender pays gas and gets approval)
    const permitReceipt = await USDC.attach(sender)
      .write.permit([emptyWallet.account.address, sender.account.address, usdcAmount, maxUint256, signature])
      .then(ReceiptVerifier.waitSuccess)

    permitReceipt.verifyEvents((ev) => {
      ev.expectCount(1).expectUSDCApproval({ owner: emptyWallet, spender: sender, value: usdcAmount })
    })

    // Sender drains all 10 USDC from wallet 1 to wallet 2 using transferFrom
    // This will revert, since it is an empty account, being fully drained
    await expect(
      USDC.attach(sender).write.transferFrom([emptyWallet.account.address, sender.account.address, usdcAmount]),
    ).rejectedWith(ContractFunctionExecutionError, 'Cannot clear balance of empty account')

    await balances.verify()

    // Now, send 1 wei of dust to the account, and try again
    await sender
      .sendTransaction({
        to: emptyWallet.account.address,
        value: 1n,
      })
      .then(ReceiptVerifier.waitSuccess)

    // This should succeed, but leave 1 wei behind
    await USDC.attach(sender)
      .write.transferFrom([emptyWallet.account.address, sender.account.address, usdcAmount])
      .then(ReceiptVerifier.waitSuccess)

    await balances
      .decrease({ emptyWallet: USDC.toNative(usdcAmount) })
      .increase({ emptyWallet: 1n })
      .verify()
  })

  it('transferFrom clearing an accounts balance', async () => {
    const { client, sender, receiver, createRandWallet } = await clients()

    // Generate a random address
    const randomWallet = await createRandWallet(parseEther('1'))

    // Grant an infinite allowance to sender
    await USDC.attach(randomWallet)
      .write.approve([sender.account.address, USDC.parseUnits('100.0')], { gas: 80000n })
      .then(ReceiptVerifier.waitSuccess)

    // Transfer away the 12 decimals of dust on this account, so that
    // the USDC transferFrom (using 6 decimals) will fully drain the account
    const divisor = 1_000_000_000_000n // 1e12
    const gas = 25_200n // intrinsic gas for a simple native transfer (21000 + 4200 for blocklist checks)

    let balance = await client.getBalance({ address: randomWallet.account.address })
    const gasPrice = await client.getGasPrice() // or pick e.g. 1_000_000_000n (1 gwei) in tests

    const gasCost = gas * gasPrice
    expect(balance > gasCost, 'insufficient native balance to pay gas').to.be.true

    // Choose value so: (balance - value - gasCost) % 1e12 == 0
    const value = (balance - gasCost) % divisor
    await randomWallet
      .sendTransaction({
        to: receiver.account.address,
        value,
        gas,
        gasPrice,
      })
      .then(ReceiptVerifier.waitSuccess)

    balance = await client.getBalance({ address: randomWallet.account.address })
    expect(balance % divisor).to.equal(0n)

    // Step 5. TransferFrom the entirety of the balance
    const usdcBalance = await USDC.attach(randomWallet).read.balanceOf([randomWallet.account.address])
    await USDC.attach(sender)
      .write.transferFrom([randomWallet.account.address, sender.account.address, usdcBalance], { gas: 80000n })
      .then(ReceiptVerifier.waitSuccess)

    // Refetch the balance
    const balanceAfter = await client.getBalance({ address: randomWallet.account.address })
    expect(balanceAfter).to.equal(0n)
  })

  it('Balances can be drained from smart contracts', async () => {
    const { client, sender } = await clients()
    const burnAmount = USDC.parseUnits('1.0')

    // Transfer funds to a smart contract
    const balanceBefore = await client.getBalance({ address: nativeTransferHelperA.address })
    expect(balanceBefore).to.equal(0n)
    await USDC.attach(sender)
      .write.transfer([nativeTransferHelperA.address, burnAmount], { gas: 80000n })
      .then(ReceiptVerifier.waitSuccess)

    // Verify the contract received the funds
    const usdcBalanceAfter = await USDC.attach(client).read.balanceOf([nativeTransferHelperA.address])
    expect(usdcBalanceAfter).to.equal(burnAmount)
    expect(await client.getBalance({ address: nativeTransferHelperA.address })).to.equal(USDC.toNative(burnAmount))

    // Now burn them
    const burnTxn = await nativeTransferHelperA.callBurn(sender, USDC.address, burnAmount).then(ReceiptVerifier.build)
    burnTxn.isSuccess()
    burnTxn.verifyEvents((ev) => {
      ev.expectNativeBurn({ from: nativeTransferHelperA.address, amount: USDC.toNative(burnAmount) })
        .expectUSDCBurn({ burner: nativeTransferHelperA.address, amount: burnAmount })
        .expectUSDCTransfer({ from: nativeTransferHelperA.address, to: zeroAddress, value: burnAmount })
        .expectAllEventsMatched()
    })

    // Verify the contract balance is zero
    const usdcBalanceFinal = await USDC.attach(client).read.balanceOf([nativeTransferHelperA.address])
    expect(usdcBalanceFinal).to.equal(0n)
    expect(await client.getBalance({ address: nativeTransferHelperA.address })).to.equal(0n)
  })

  it('EIP-2612 permit transfer', async () => {
    const { client, usdc, receiver, sender } = await clients()
    const value = USDC.parseUnits('78.39192')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      totalSupply: () => usdc.totalSupply(),
      senderUSDC: () => usdc.balanceOf([sender.account.address]),
      receiverUSDC: () => usdc.balanceOf([receiver.account.address]),
      senderAllowance: () => usdc.allowance([sender.account.address, receiver.account.address]),
    })

    const signature = await signPermit({
      client,
      wallet: sender,
      permitAmount: value,
      spenderAddress: receiver.account.address,
    })

    const permitReceipt = await USDC.attach(receiver)
      .write.permit([sender.account.address, receiver.account.address, value, maxUint256, signature])
      .then(ReceiptVerifier.waitSuccess)

    permitReceipt.verifyEvents((ev) => {
      ev.expectCount(1).expectUSDCApproval({ owner: sender, spender: receiver, value })
    })

    await balances
      .decrease({
        receiver: permitReceipt.totalFee(),
        receiverUSDC: value + USDC.fromNative(permitReceipt.totalFee()).notAccurate,
      })
      .verifyWithOverride({
        senderAllowance: () => value,
        receiverUSDC: (_, after) => USDC.fromNative(after.receiver).roundDown,
      })

    const transferReceipt = await USDC.attach(receiver)
      .write.transferFrom([sender.account.address, receiver.account.address, value])
      .then(ReceiptVerifier.waitSuccess)
    transferReceipt.verifyEvents((ev) => {
      ev.expectCount(2)
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(value) })
        .expectUSDCTransfer({ from: sender, to: receiver, value })
    })

    await balances
      .increase({
        receiver: USDC.toNative(value),
        receiverUSDC: value,
      })
      .decrease({
        sender: USDC.toNative(value),
        receiver: transferReceipt.totalFee(),
        senderUSDC: value,
        receiverUSDC: USDC.fromNative(transferReceipt.totalFee()).notAccurate,
      })
      .verifyWithOverride({
        senderAllowance: () => 0n,
        receiverUSDC: (_, after) => USDC.fromNative(after.receiver).roundDown,
      })
  })

  describe('different call types', () => {
    let helperAddress: Address
    before(async () => {
      const { client, operator, sender, admin } = await clients()
      const amount = USDC.parseUnits('1')
      const helper = await CallHelper.deploy(sender, client)
      helperAddress = helper.address

      // Initialize the helper contract and add it as a minter.
      await USDC.attach(admin).write.configureMinter([helper.address, amount]).then(ReceiptVerifier.waitSuccess)

      await USDC.attach(operator).write.mint([helper.address, amount]).then(ReceiptVerifier.waitSuccess)
    })

    const testCallType = async (tc: {
      functionName: 'staticCall' | 'delegateCall' | 'execute' | 'callCode'
      fn: 'mint' | 'transfer'
    }) => {
      const { client, usdc, receiver, totalSupply, sender } = await clients()
      const helper = CallHelper.attach(sender, helperAddress)
      const amount = USDC.parseUnits('0.000001')

      const balances = await balancesSnapshot(client, {
        sender,
        helper,
        totalSupply,
        receiverUSDC: () => usdc.balanceOf([receiver.account.address]),
      })

      const receipt = await sender
        .sendTransaction({
          to: helper.address,
          data: CallHelper.encodeNested({
            fn: tc.functionName,
            target: USDC.address,
            data: encodeFunctionData({ abi: USDC.abi, functionName: tc.fn, args: [receiver.account.address, amount] }),
          }),
        })
        .then(ReceiptVerifier.waitSuccess)

      const verifySuccess = async () => {
        if (tc.fn === 'mint') {
          receipt.verifyEvents((ev) => {
            ev.expectNativeMint({ recipient: receiver, amount: USDC.toNative(amount) })
              .expectUSDCMint({ minter: helper, to: receiver, amount: amount })
              .expectUSDCTransfer({ from: zeroAddress, to: receiver, value: amount })
              .expectExecutionResult({ helper, success: true, result: toBytes32(1n) })
              .expectAllEventsMatched()
          })
          await balances
            .decrease({ sender: receipt.totalFee() })
            .increase({ receiverUSDC: amount, totalSupply: USDC.toNative(amount) })
            .verify()
        } else {
          receipt.verifyEvents((ev) => {
            ev.expectNativeTransfer({ from: helper, to: receiver, amount: USDC.toNative(amount) })
              .expectUSDCTransfer({ from: helper, to: receiver, value: amount })
              .expectExecutionResult({ helper, success: true, result: toBytes32(1n) })
              .expectAllEventsMatched()
          })
          await balances
            .decrease({ sender: receipt.totalFee(), helper: USDC.toNative(amount) })
            .increase({ receiverUSDC: amount })
            .verify()
        }
      }

      return { receipt, balances, helper, amount, sender, receiver, verifySuccess }
    }

    it('staticCall USDC.transfer', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'staticCall', fn: 'transfer' })
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({
          helper,
          success: false,
          result: CallHelper.encodeRevertMessage('State change during static call'),
        })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('delegateCall USDC.transfer', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'delegateCall', fn: 'transfer' })
      // since the impl address is not set, the delegate call will call to zero address
      // which will sucess execute with empty return
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({ helper, success: true, result: '0x' })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('callCode USDC.transfer', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'callCode', fn: 'transfer' })
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({ helper, success: true, result: '0x' })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('indirect call USDC.transfer', async () => {
      const { verifySuccess } = await testCallType({ functionName: 'execute', fn: 'transfer' })
      await verifySuccess()
    })

    it('staticCall USDC.mint', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'staticCall', fn: 'mint' })
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({ helper, success: false, result: '0x' })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('delegateCall USDC.mint', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'delegateCall', fn: 'mint' })
      // since the impl address is not set, the delegate call will call to zero address
      // which will sucess execute with empty return
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({ helper, success: true, result: '0x' })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('callCode USDC.mint', async () => {
      const { receipt, balances, helper } = await testCallType({ functionName: 'callCode', fn: 'mint' })
      receipt.verifyEvents((ev) => {
        ev.expectCount(1).expectExecutionResult({ helper, success: true, result: '0x' })
      })
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })
    it('indirect call USDC.mint', async () => {
      const { verifySuccess } = await testCallType({ functionName: 'execute', fn: 'mint' })
      await verifySuccess()
    })
  })

  describe('precompiles', () => {
    let helper: Awaited<ReturnType<typeof CallHelper.deploy>>
    before(async () => {
      const { sender, client } = await clients()
      helper = await CallHelper.deploy(sender, client, parseEther('0.0001'))
    })

    const precompiles = {
      nativeCoinAuthority: NativeCoinAuthority.address,
      nativeCoinControl: NativeCoinControl.address,
    }

    it('should not call precompiles directly', async () => {
      const { sender, client } = await clients()
      const amount = parseEther('0.000000001')
      const balances = await balancesSnapshot(client, { sender, ...precompiles })

      for (const addr of Object.values(precompiles)) {
        await expect(sender.sendTransaction({ to: addr, value: amount })).rejectedWith(
          TransactionExecutionError,
          'Execution reverted',
        )
        await balances.verify()
      }

      for (const addr of Object.values(precompiles)) {
        const receipt = await sender
          .sendTransaction({ to: addr, value: amount, gas: 1000000n })
          .then(ReceiptVerifier.wait)
        receipt.isReverted().verifyNoEvents()
        await balances.decrease({ sender: receipt.totalFee() }).verify()
      }
    })

    it('catch the static revert error', async () => {
      const { sender, client } = await clients()
      const amount1 = parseEther('0.0000000001')
      const amount2 = parseEther('0.0000000002')
      const amount3 = parseEther('0.0000000003')
      const balances = await balancesSnapshot(client, { sender, helper: helper.address, ...precompiles })

      for (const addr of Object.values(precompiles)) {
        const receipt = await sender
          .sendTransaction({
            to: helper.address,
            data: CallHelper.encodeNested({
              fn: 'executeBatch',
              calls: [
                {
                  target: helper.address,
                  data: { fn: 'staticCall', target: addr, data: { fn: 'execute', target: addr, value: amount2 } },
                },
                {
                  target: sender.account.address,
                  value: amount3,
                },
              ],
            }),
            value: amount1,
          })
          .then(ReceiptVerifier.waitSuccess)
        receipt.verifyEvents((ev) => {
          ev.expectNativeTransfer({ from: sender, to: helper, amount: amount1 })
            .expectExecutionResult({
              helper,
              success: false,
              result: CallHelper.encodeRevertMessage('Invalid selector'),
            })
            .expectExecutionResult({
              helper: helper.address,
              success: true,
              nested: { success: false, result: CallHelper.encodeRevertMessage('Invalid selector') },
            })
            .expectNativeTransfer({ from: helper, to: sender, amount: amount3 })
            .expectExecutionResult({ helper: helper.address, success: true, result: '0x' })
            .expectAllEventsMatched()
        })
        await balances
          .decrease({ sender: receipt.totalFee() + amount1, helper: amount3 })
          .increase({ sender: amount3, helper: amount1 })
          .verify()
      }
    })

    it('gas used for static revert', async () => {
      const { client, sender, admin } = await getClients()
      const helper = await CallHelper.deploy(sender, client)

      // setup the helper as the minter
      await USDC.attach(admin).write.configureMinter([helper.address, 1n])

      const receipt = await sender
        .sendTransaction({
          to: helper.address,
          data: CallHelper.encodeNested({
            fn: 'staticCall',
            target: USDC.address,
            data: encodeFunctionData({ abi: USDC.abi, functionName: 'mint', args: [sender.account.address, 1n] }),
          }),
          gas: 160000n,
        })
        .then(ReceiptVerifier.waitSuccess)
      receipt.verifyGasUsedApproximately(160318n).verifyEvents((ev) => {
        ev.expectExecutionResult({ helper, success: false, result: '0x' })
      })
    })
  })

  describe('initialization protection', () => {
    it('all initialize functions are protected', async () => {
      const { client, admin } = await clients()
      const initAbi = parseAbi([
        'function initialize(string,string,string,uint8,address,address,address,address)',
        'function initializeV2(string calldata newName)',
        'function initializeV2_1(address lostAndFound)',
        'function initializeV2_2(address[] calldata accountsToBlacklist, string calldata newSymbol)',
      ])

      // Verify contract is at version 3 (fully initialized)
      const version = await client.getStorageAt({ address: USDC.address, slot: toHex(18n, { size: 32 }) })
      expect(version).to.be.eq(toHex(3n, { size: 32 }))

      // Try all initialize functions - all should fail
      await expect(
        admin.sendTransaction({
          to: USDC.address,
          data: encodeFunctionData({
            abi: initAbi,
            functionName: 'initialize',
            args: [
              'USDC',
              'USDC',
              'USD',
              6,
              admin.account.address,
              admin.account.address,
              admin.account.address,
              admin.account.address,
            ],
          }),
        }),
      ).to.be.rejected

      await expect(
        admin.sendTransaction({
          to: USDC.address,
          data: encodeFunctionData({ abi: initAbi, functionName: 'initializeV2', args: ['USDC'] }),
        }),
      ).to.be.rejected

      await expect(
        admin.sendTransaction({
          to: USDC.address,
          data: encodeFunctionData({ abi: initAbi, functionName: 'initializeV2_1', args: [admin.account.address] }),
        }),
      ).to.be.rejected

      await expect(
        admin.sendTransaction({
          to: USDC.address,
          data: encodeFunctionData({ abi: initAbi, functionName: 'initializeV2_2', args: [[], 'USDC'] }),
        }),
      ).to.be.rejected
    })
  })

  describe('inner revert handling with ERC20 precompile', () => {
    let helperOneAddress: Address
    let helperTwoAddress: Address
    before(async () => {
      const { client, sender, operator, admin } = await clients()
      const mintAmount = parseEther('100')
      const mintAllowanceAmount = parseEther('100')

      const helperOne = await CallHelper.deploy(sender, client)
      helperOneAddress = helperOne.address
      const helperTwo = await CallHelper.deploy(sender, client)
      helperTwoAddress = helperTwo.address

      await USDC.attach(operator).write.mint([helperOneAddress, mintAmount]).then(ReceiptVerifier.waitSuccess)
      await USDC.attach(operator).write.mint([helperTwoAddress, mintAmount]).then(ReceiptVerifier.waitSuccess)
      await USDC.attach(admin)
        .write.configureMinter([helperOneAddress, mintAllowanceAmount])
        .then(ReceiptVerifier.waitSuccess)
      await USDC.attach(admin)
        .write.configureMinter([helperTwoAddress, mintAllowanceAmount])
        .then(ReceiptVerifier.waitSuccess)
    })

    // Call HelperOne which ERC20 transfers to an address
    // HelperOne then calls HelperTwo to transfer to the same address, and then reverts
    // HelperOne allows the failure of HelperTwo's call, so the overall transaction succeeds
    it('reverted inner ERC20 transfer call to account previously updated does not update account state', async () => {
      const { client, sender } = await clients()
      const recipient = privateKeyToAccount(generatePrivateKey())
      const amount = USDC.parseUnits('1')

      // HelperOne transfers to recipient
      const initialTransferCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'transfer',
        args: [recipient.address, amount],
      })

      // HelperOne calls HelperTwo to transfer and then revert
      const transferAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [
          USDC.address,
          encodeFunctionData({ abi: USDC.abi, functionName: 'transfer', args: [recipient.address, amount] }),
        ],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        recipient: recipient.address,
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialTransferCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: transferAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne transfers to recipient
          .expectNativeTransfer({ from: helperOneAddress, to: recipient.address, amount: parseEther('1') })
          .expectUSDCTransfer({ from: helperOneAddress, to: recipient.address, value: USDC.parseUnits('1') })

          // HelperOne's successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      await balances
        .decrease({ helperOne: USDC.toNative(amount) })
        .increase({ recipient: USDC.toNative(amount) })
        .verify()
    })

    // 3 calls:
    // 1. HelperOne ERC20 transfers to an address
    // 2. HelperOne then calls HelperTwo to transfer to the same address, and then reverts. Result is ignored.
    // 3. HelperOne ERC20 trasfers to an address again
    it('reverted inner ERC20 transfer call to account previously updated does not update account state when surrounded by successful calls', async () => {
      const { client, sender } = await clients()
      const recipient = privateKeyToAccount(generatePrivateKey())
      const initialAmount = USDC.parseUnits('1')
      const failedTransferAmount = USDC.parseUnits('3')
      const finalTransferAmount = USDC.parseUnits('5')

      // HelperOne transfers to recipient
      const initialTransferCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'transfer',
        args: [recipient.address, initialAmount],
      })

      // HelperOne calls HelperTwo to transfer and then revert
      const transferAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [
          USDC.address,
          encodeFunctionData({
            abi: USDC.abi,
            functionName: 'transfer',
            args: [recipient.address, failedTransferAmount],
          }),
        ],
      })

      // HelperOne transfers to recipient
      const finalTransferCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'transfer',
        args: [recipient.address, finalTransferAmount],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        recipient: recipient.address,
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialTransferCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: transferAndRevertCallData, allowFailure: true, value: 0n },
            { target: USDC.address, callData: finalTransferCallData, allowFailure: false, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne transfers to recipient
          .expectNativeTransfer({ from: helperOneAddress, to: recipient.address, amount: USDC.toNative(initialAmount) })
          .expectUSDCTransfer({ from: helperOneAddress, to: recipient.address, value: initialAmount })

          // HelperOne's successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })

          // HelperOne final successful transfer to recipient
          .expectNativeTransfer({
            from: helperOneAddress,
            to: recipient.address,
            amount: USDC.toNative(finalTransferAmount),
          })
          .expectUSDCTransfer({ from: helperOneAddress, to: recipient.address, value: finalTransferAmount })

          // HelperOne's final successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })
          .expectAllEventsMatched()
      })

      await balances
        .decrease({ helperOne: USDC.toNative(initialAmount + finalTransferAmount) })
        .increase({ recipient: USDC.toNative(initialAmount + finalTransferAmount) })
        .verify()
    })

    // Call HelperOne which ERC20 transfers to an address
    // HelperOne then calls HelperTwo to transfer to a different address, and then reverts
    // HelperOne allows the failure of HelperTwo's call, so the overall transaction succeeds
    it('reverted inner ERC20 transfer call to account not previously updated does not update account state', async () => {
      const { client, sender } = await clients()
      const recipientOne = privateKeyToAccount(generatePrivateKey())
      const recipientTwo = privateKeyToAccount(generatePrivateKey())

      const amount = USDC.parseUnits('1')

      // HelperOne transfers to recipientOne
      const initialTransferCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'transfer',
        args: [recipientOne.address, amount],
      })

      // HelperOne calls HelperTwo to transfer to recipientTwo and then revert
      const transferAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [
          USDC.address,
          encodeFunctionData({ abi: USDC.abi, functionName: 'transfer', args: [recipientTwo.address, amount] }),
        ],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        recipientOne: recipientOne.address,
        recipientTwo: recipientTwo.address,
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialTransferCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: transferAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne transfers to recipient
          .expectNativeTransfer({ from: helperOneAddress, to: recipientOne.address, amount: parseEther('1') })
          .expectUSDCTransfer({ from: helperOneAddress, to: recipientOne.address, value: USDC.parseUnits('1') })

          // HelperOne's successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      await balances
        .decrease({ helperOne: USDC.toNative(amount) })
        .increase({ recipientOne: USDC.toNative(amount) })
        .verify()
    })

    // Call HelperOne which ERC20 mints() to an address
    // HelperOne then calls HelperTwo to mint to the same address, and then reverts
    // HelperOne allows the failure of HelperTwo's call, so the overall transaction succeeds
    it('reverted inner ERC20 mint call to account previously updated does not update account state', async () => {
      const { client, sender, usdc } = await clients()
      const recipient = privateKeyToAccount(generatePrivateKey())

      const amount = USDC.parseUnits('1')

      // HelperOne mints to recipient
      const initialMintCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'mint',
        args: [recipient.address, amount],
      })

      // HelperOne calls HelperTwo to mint to recipient and then revert
      const mintAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [
          USDC.address,
          encodeFunctionData({ abi: USDC.abi, functionName: 'mint', args: [recipient.address, amount] }),
        ],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        recipient: recipient.address,
        totalSupplyUSDC: () => usdc.totalSupply(),
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialMintCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: mintAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne transfers to recipient
          .expectNativeMint({ recipient: recipient.address, amount: parseEther('1') })
          .expectUSDCMint({ to: recipient.address, minter: helperOneAddress, amount: USDC.parseUnits('1') })
          .expectUSDCTransfer({ from: zeroAddress, to: recipient.address, value: USDC.parseUnits('1') })

          // HelperOne's successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      await balances
        .increase({
          recipient: USDC.toNative(amount),
          totalSupplyUSDC: amount,
        })
        .verify()
    })

    // Call HelperOne which ERC20 mints() to addressOne
    // HelperOne then calls HelperTwo to mint to addressTwo, and then reverts
    // HelperOne allows the failure of HelperTwo's call, so the overall transaction succeeds
    it('reverted inner ERC20 mint call to account NOT previously updated does not update account state', async () => {
      const { client, sender, usdc } = await clients()
      const recipientOne = privateKeyToAccount(generatePrivateKey())
      const recipientTwo = privateKeyToAccount(generatePrivateKey())

      const amount = USDC.parseUnits('1')

      // HelperOne mints to recipient
      const initialMintCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'mint',
        args: [recipientOne.address, amount],
      })

      // HelperOne calls HelperTwo to mint to recipient and then revert
      const mintAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [
          USDC.address,
          encodeFunctionData({ abi: USDC.abi, functionName: 'mint', args: [recipientTwo.address, amount] }),
        ],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        recipientOne: recipientOne.address,
        recipientTwo: recipientTwo.address,
        totalSupplyUSDC: () => usdc.totalSupply(),
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialMintCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: mintAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne transfers to recipient
          .expectNativeMint({ recipient: recipientOne.address, amount: parseEther('1') })
          .expectUSDCMint({ to: recipientOne.address, minter: helperOneAddress, amount: USDC.parseUnits('1') })
          .expectUSDCTransfer({ from: zeroAddress, to: recipientOne.address, value: USDC.parseUnits('1') })

          // HelperOne's successful transfer
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: toBytes32(1n) })

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      await balances
        .increase({
          recipientOne: USDC.toNative(amount),
          totalSupplyUSDC: amount,
        })
        .verify()
    })

    // Call HelperOne which ERC20 burn() from its balance
    // HelperOne then calls itself to burn from its balance, and then reverts
    // HelperOne allows the failure of the inner call, so the overall transaction succeeds
    it('reverted inner ERC20 burn call to same address does not update account state', async () => {
      const { client, sender, usdc } = await clients()

      const amount = USDC.parseUnits('1')

      // HelperOne burn
      const initialBurnCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'burn',
        args: [amount],
      })

      // HelperOne calls itself to burn and then revert
      const burnAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [USDC.address, encodeFunctionData({ abi: USDC.abi, functionName: 'burn', args: [amount] })],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        zero: zeroAddress,
        totalSupplyUSDC: () => usdc.totalSupply(),
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialBurnCallData, allowFailure: false, value: 0n },
            { target: helperOneAddress, callData: burnAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne burns amount
          .expectNativeBurn({ from: helperOneAddress, amount: USDC.toNative(amount) })
          .expectUSDCBurn({ burner: helperOneAddress, amount })
          .expectUSDCTransfer({ from: helperOneAddress, to: zeroAddress, value: amount })

          // HelperOne's successful burn
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: '0x' }) // burn() does not return a bool

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      // Note: in ArcHardfork::zero4, burns no longer transfer to the zero address
      await balances
        .decrease({
          helperOne: USDC.toNative(amount),
          totalSupplyUSDC: amount,
        })
        .verify()
    })

    // Call HelperOne which ERC20 burn() from its balance
    // HelperOne then calls HelperTwo to burn from its balance, and then reverts
    // HelperOne allows the failure of HelperTwo's call, so the overall transaction succeeds
    it('reverted inner ERC20 burn call to different address does not update account state', async () => {
      const { client, sender, usdc } = await clients()

      const amount = USDC.parseUnits('1')

      // HelperOne burn
      const initialBurnCallData = encodeFunctionData({
        abi: USDC.abi,
        functionName: 'burn',
        args: [amount],
      })

      // HelperOne calls HelperTwo to burn and then revert
      const burnAndRevertCallData = encodeFunctionData({
        abi: CallHelper.abi,
        functionName: 'callAndRevert',
        args: [USDC.address, encodeFunctionData({ abi: USDC.abi, functionName: 'burn', args: [amount] })],
      })

      const balances = await balancesSnapshot(client, {
        helperOne: helperOneAddress,
        helperTwo: helperTwoAddress,
        totalSupplyUSDC: () => usdc.totalSupply(),
      })

      const receipt = await CallHelper.attach(sender, helperOneAddress)
        .write.executeBatch([
          [
            { target: USDC.address, callData: initialBurnCallData, allowFailure: false, value: 0n },
            { target: helperTwoAddress, callData: burnAndRevertCallData, allowFailure: true, value: 0n },
          ],
        ])
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev
          // HelperOne burns amount
          .expectNativeBurn({ from: helperOneAddress, amount: USDC.toNative(amount) })
          .expectUSDCBurn({ burner: helperOneAddress, amount })
          .expectUSDCTransfer({ from: helperOneAddress, to: zeroAddress, value: amount })

          // HelperOne's successful burn
          .expectExecutionResult({ helper: helperOneAddress, success: true, result: '0x' }) // burn() does not return a bool

          // HelperOne's reverted (and ignored) call to HelperTwo
          .expectExecutionResult({
            helper: helperOneAddress,
            success: false,
            result: CallHelper.encodeRevertMessage('Intentional revert after call'),
          })
          .expectAllEventsMatched()
      })

      await balances
        .decrease({
          helperOne: USDC.toNative(amount),
          totalSupplyUSDC: amount,
        })
        .verify()
    })
  })
})
