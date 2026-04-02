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

import hre from 'hardhat'
import { expect } from 'chai'
import { balancesSnapshot, NativeCoinAuthority, ReceiptVerifier, getClients } from '../helpers'
import { USDC } from '../helpers/FiatToken'
import { CallHelper } from '../helpers/CallHelper'
import { callFromAddress, memoAddress, multicall3FromAddress } from '../../scripts/genesis'
import { encodeFunctionData, erc20Abi, keccak256, pad, toHex, maxUint256, Address, parseAbi, parseEther } from 'viem'

const memoArtifact = hre.artifacts.readArtifactSync('Memo')
const multicall3FromArtifact = hre.artifacts.readArtifactSync('Multicall3From')

const memoReadAbi = parseAbi(['function memoIndex() view returns (uint256)'])

const callFromAbi = parseAbi([
  'function callFrom(address sender, address target, bytes calldata data) external returns (bool success, bytes memory returnData)',
])

const clients = async () => {
  const { client, sender, receiver, ...rest } = await getClients()
  const usdc = USDC.attach(client).read
  const totalSupply = async () => NativeCoinAuthority.totalSupply(client)
  return { ...rest, client, sender, receiver, usdc, totalSupply }
}

const readMemoIndex = async (): Promise<bigint> => {
  const { client } = await getClients()
  return client.readContract({
    address: memoAddress,
    abi: memoReadAbi,
    functionName: 'memoIndex',
  })
}

const encodeUSDCTransfer = (to: Address, amount: bigint) =>
  encodeFunctionData({ abi: erc20Abi, functionName: 'transfer', args: [to, amount] })

const encodeMemo = (target: Address, data: `0x${string}`, memoId: `0x${string}`, memo: `0x${string}`) =>
  encodeFunctionData({
    abi: memoArtifact.abi,
    functionName: 'memo',
    args: [target, data, memoId, memo],
  })

const encodeAggregate3 = (calls: { target: Address; allowFailure: boolean; callData: `0x${string}` }[]) =>
  encodeFunctionData({
    abi: multicall3FromArtifact.abi,
    functionName: 'aggregate3',
    args: [calls],
  })

const amt1 = USDC.parseUnits('0.001')
const amt2 = USDC.parseUnits('0.002')
const amt3 = USDC.parseUnits('0.003')
const memoId1 = pad(toHex(1), { size: 32 })
const memoId2 = pad(toHex(2), { size: 32 })
const memo1 = toHex('payment-1')
const memo2 = toHex('payment-2')

let callHelper: Awaited<ReturnType<typeof CallHelper.deploy>>

before(async () => {
  const { client, sender } = await clients()
  callHelper = await CallHelper.deploy(sender, client, parseEther('1'))
})

describe('Multicall3From', () => {
  // Plain batch execution — no memo involvement.
  // sender → Multicall3From.aggregate3 → callFrom(sender, USDC, transfer) × 2
  it('batch-only', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const totalTransferred = USDC.toNative(amt1 + amt2)

    const balances = await balancesSnapshot(client, { sender, receiver, totalSupply })

    const callData = encodeAggregate3([
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt1) },
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt2) },
    ])

    const receipt = await sender
      .sendTransaction({ to: multicall3FromAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt1) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt1 })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt2) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt2 })
        .expectAllEventsMatched()
    })

    await balances
      .decrease({ sender: totalTransferred + receipt.totalFee() })
      .increase({ receiver: totalTransferred })
      .verify()
  })
})

describe('Memo', () => {
  // Direct USDC transfer via Memo — callFrom preserves msg.sender=sender.
  // sender → Memo.memo(USDC, transfer) → callFrom(sender, USDC, transfer)
  it('direct call to Memo', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndex = await readMemoIndex()
    const amount = USDC.parseUnits('0.001')
    const nativeAmount = USDC.toNative(amount)
    const memoId = keccak256(toHex('test-memo-id'))
    const memo = toHex('hello memo')
    const transferData = encodeUSDCTransfer(receiver.account.address, amount)
    const callDataHash = keccak256(transferData)

    const balances = await balancesSnapshot(client, { sender, receiver })

    const callData = encodeMemo(USDC.address, transferData, memoId, memo)
    const receipt = await sender
      .sendTransaction({ to: memoAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex })
        .expectNativeTransfer({ from: sender, to: receiver, amount: nativeAmount })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amount })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash,
          memoId,
          memo,
          memoIndex,
        })
        .expectAllEventsMatched()
    })

    await balances
      .increase({ receiver: nativeAmount })
      .decrease({ sender: nativeAmount + receipt.totalFee() })
      .verify()
  })

  // Indirect call through CallHelper — callFrom spoofs CallHelper as sender, transfers from its balance.
  // sender → CallHelper.execute → Memo → callFrom(CallHelper, USDC, transfer)
  it('indirect call via CallHelper', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndex = await readMemoIndex()
    const amount = USDC.parseUnits('0.001')
    const nativeAmount = USDC.toNative(amount)
    const memoId = keccak256(toHex('indirect-memo'))
    const memo = toHex('indirect hello')
    const transferData = encodeUSDCTransfer(receiver.account.address, amount)
    const callDataHash = keccak256(transferData)
    const memoData = encodeMemo(USDC.address, transferData, memoId, memo)

    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      callHelper: callHelper.address,
    })

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.execute([memoAddress, memoData, 0n])
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex })
        .expectNativeTransfer({ from: callHelper.address, to: receiver, amount: nativeAmount })
        .expectUSDCTransfer({ from: callHelper.address, to: receiver, value: amount })
        .expectMemo({
          sender: callHelper.address,
          target: USDC.address,
          callDataHash,
          memoId,
          memo,
          memoIndex,
        })
        .expectExecutionResult({ helper: callHelper.address, success: true, result: '0x' })
        .expectAllEventsMatched()
    })

    await balances
      .increase({ receiver: nativeAmount })
      .decrease({ callHelper: nativeAmount, sender: receipt.totalFee() })
      .verify()
  })

  // 3-deep recursive memo nesting, innermost does transferFrom.
  // sender → Memo(outer) → Memo(mid) → Memo(inner) → USDC.transferFrom
  // Memo events unwind innermost-first; memoIndex += 3.
  it('recursion 3 times then transferFrom', async () => {
    const { client, sender, receiver, usdc } = await clients()

    const startMemoIndex = await readMemoIndex()
    const amount = USDC.parseUnits('0.0003')
    const nativeAmount = USDC.toNative(amount)
    const memoIdR1 = keccak256(toHex('recurse-1'))
    const memoIdR2 = keccak256(toHex('recurse-2'))
    const memoIdR3 = keccak256(toHex('recurse-3'))

    // sender approves sender (self) for transferFrom.
    await USDC.attach(sender).write.approve([sender.account.address, amount]).then(ReceiptVerifier.waitSuccess)

    const balances = await balancesSnapshot(client, { sender, receiver })

    const transferFromData = encodeFunctionData({
      abi: USDC.abi,
      functionName: 'transferFrom',
      args: [sender.account.address, receiver.account.address, amount],
    })

    // innermost: memo(FiatTokenProxy, transferFrom, memoId3, "inner")
    const innerData = encodeMemo(USDC.address, transferFromData, memoIdR3, toHex('inner'))

    // middle: memo(Memo, innerData, memoId2, "mid")
    const middleData = encodeMemo(memoAddress, innerData, memoIdR2, toHex('mid'))

    // outer: memo(Memo, middleData, memoId1, "outer")
    const outerData = encodeMemo(memoAddress, middleData, memoIdR1, toHex('outer'))
    const receipt = await sender
      .sendTransaction({ to: memoAddress, data: outerData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex: startMemoIndex })
        .expectBeforeMemo({ memoIndex: startMemoIndex + 1n })
        .expectBeforeMemo({ memoIndex: startMemoIndex + 2n })
        .expectNativeTransfer({ from: sender, to: receiver, amount: nativeAmount })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amount })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash: keccak256(transferFromData),
          memoId: memoIdR3,
          memo: toHex('inner'),
          memoIndex: startMemoIndex + 2n,
        })
        .expectMemo({
          sender,
          target: memoAddress,
          callDataHash: keccak256(innerData),
          memoId: memoIdR2,
          memo: toHex('mid'),
          memoIndex: startMemoIndex + 1n,
        })
        .expectMemo({
          sender,
          target: memoAddress,
          callDataHash: keccak256(middleData),
          memoId: memoIdR1,
          memo: toHex('outer'),
          memoIndex: startMemoIndex,
        })
        .expectAllEventsMatched()
    })

    const allowanceAfter = await usdc.allowance([sender.account.address, sender.account.address])
    expect(allowanceAfter).to.eq(0n)

    await balances
      .increase({ receiver: nativeAmount })
      .decrease({ sender: nativeAmount + receipt.totalFee() })
      .verify()
  })

  // callFrom targeting an EOA with empty data succeeds as a no-op — only memo events, no transfers.
  // sender → Memo(receiver, "0x") → callFrom(sender, receiver, "0x") → success
  it('callFrom target is EOA — succeeds with no transfer', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndex = await readMemoIndex()
    const memoId = keccak256(toHex('eoa-target'))
    const memo = toHex('call eoa')

    const balances = await balancesSnapshot(client, { sender, receiver })

    const callData = encodeMemo(receiver.account.address, '0x', memoId, memo)
    const receipt = await sender
      .sendTransaction({ to: memoAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex })
        .expectMemo({
          sender,
          target: receiver,
          callDataHash: keccak256('0x'),
          memoId,
          memo,
          memoIndex,
        })
        .expectAllEventsMatched()
    })

    // Only gas spent, no transfers
    await balances.decrease({ sender: receipt.totalFee() }).verify()
  })

  // Child reverts — journal rollback undoes memoIndex++ and events. Only ExecutionResult survives.
  // sender → CallHelper.execute → Memo → callFrom → CallHelper.revertWithString → REVERT
  it('child reverts — state and memoIndex rolled back', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndexBefore = await readMemoIndex()
    const revertData = encodeFunctionData({
      abi: CallHelper.abi,
      functionName: 'revertWithString',
      args: ['intentional revert'],
    })
    const memoData = encodeMemo(
      callHelper.address,
      revertData,
      keccak256(toHex('revert-test')),
      toHex('will revert'),
    )

    const balances = await balancesSnapshot(client, { sender, receiver })

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.execute([memoAddress, memoData, 0n])
      .then(ReceiptVerifier.waitSuccess)

    // Only the CallHelper ExecutionResult event survives (Memo events rolled back).
    expect(receipt.receipt.logs).to.have.lengthOf(1)
    receipt.verifyEvents((ev) => {
      const result = ev.getExecutionResult(0)
      expect(result.slice(0, 10)).to.eq('0xed1966a2') // MemoFailed selector
      ev.setLogIndex(1).expectAllEventsMatched()
    })

    // memoIndex unchanged — the increment was inside the reverted frame
    const memoIndexAfter = await readMemoIndex()
    expect(memoIndexAfter).to.eq(memoIndexBefore)

    // No balance changes except gas
    await balances.decrease({ sender: receipt.totalFee() }).verify()
  })

  // Custom error propagation: ErrorMessage wraps inside MemoFailed through multiple layers.
  // sender → CallHelper.execute → Memo → callFrom → CallHelper.revertWithError → REVERT
  it('child reverts with custom error — error propagated through MemoFailed', async () => {
    const { client, sender } = await clients()

    const revertData = encodeFunctionData({
      abi: CallHelper.abi,
      functionName: 'revertWithError',
      args: ['custom error message'],
    })
    const memoData = encodeMemo(
      callHelper.address,
      revertData,
      keccak256(toHex('error-prop')),
      toHex('error test'),
    )

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.execute([memoAddress, memoData, 0n])
      .then(ReceiptVerifier.waitSuccess)

    expect(receipt.receipt.logs).to.have.lengthOf(1)
    receipt.verifyEvents((ev) => {
      const result = ev.getExecutionResult(0)
      // Outer: MemoFailed(bytes) selector = 0xed1966a2
      expect(result.slice(0, 10)).to.eq('0xed1966a2')
      // The inner bytes contain ErrorMessage("custom error message") selector = 0xa183e9a5
      expect(result).to.contain('a183e9a5')
      ev.setLogIndex(1).expectAllEventsMatched()
    })
  })

  // Two sequential txs: CALL succeeds, then STATICCALL reverts. State from first persists.
  // tx1: sender → CallHelper.execute → Memo → callFrom → USDC.transfer (success)
  // tx2: sender → CallHelper.staticCall → Memo → REVERT (read-only)
  it('call then static call — first succeeds, second reverts', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndex = await readMemoIndex()
    const amount = USDC.parseUnits('0.001')
    const nativeAmount = USDC.toNative(amount)
    const memoIdCall = keccak256(toHex('call-then-static'))
    const memo = toHex('first call')
    const transferData = encodeUSDCTransfer(receiver.account.address, amount)
    const callDataHash = keccak256(transferData)
    const memoData = encodeMemo(USDC.address, transferData, memoIdCall, memo)

    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      callHelper: callHelper.address,
    })

    // Transaction 1: execute (CALL) succeeds
    const receipt1 = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.execute([memoAddress, memoData, 0n])
      .then(ReceiptVerifier.waitSuccess)

    receipt1.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex })
        .expectNativeTransfer({ from: callHelper.address, to: receiver, amount: nativeAmount })
        .expectUSDCTransfer({ from: callHelper.address, to: receiver, value: amount })
        .expectMemo({
          sender: callHelper.address,
          target: USDC.address,
          callDataHash,
          memoId: memoIdCall,
          memo,
          memoIndex,
        })
        .expectExecutionResult({ helper: callHelper.address, success: true, result: '0x' })
        .expectAllEventsMatched()
    })

    // Transaction 2: staticCall should fail
    const receipt2 = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.staticCall([memoAddress, memoData])
      .then(ReceiptVerifier.waitSuccess)

    receipt2.verifyEvents((ev) => {
      ev.expectExecutionResult({ helper: callHelper.address, success: false, result: '0x' }).expectAllEventsMatched()
    })

    // Only the first call transferred funds
    await balances
      .increase({ receiver: nativeAmount })
      .decrease({
        callHelper: nativeAmount,
        sender: receipt1.totalFee() + receipt2.totalFee(),
      })
      .verify()
  })

  // DELEGATECALL runs Memo code in CallHelper's context (address(this) = CallHelper).
  // The CALL to callFrom originates from CallHelper, which is not allowlisted — rejected.
  // sender → CallHelper.delegateCall → DELEGATECALL → callFrom → "unauthorized caller"
  it('delegate call should revert', async () => {
    const { client, sender, receiver } = await clients()
    const amount = USDC.parseUnits('0.001')
    const transferData = encodeUSDCTransfer(receiver.account.address, amount)
    const memoData = encodeMemo(
      USDC.address,
      transferData,
      keccak256(toHex('delegate-memo')),
      toHex('delegate'),
    )

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.delegateCall([memoAddress, memoData])
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectExecutionResult({
        helper: callHelper.address,
        success: false,
        revertString: 'unauthorized caller',
      }).expectAllEventsMatched()
    })
  })

  // EOA directly calling callFrom is not allowlisted — reverts with "unauthorized caller".
  // sender → callFrom(sender, receiver, "0x") → REVERT
  it('unauthorized direct call to callFrom precompile reverts', async () => {
    const { sender, receiver } = await clients()

    const callFromData = encodeFunctionData({
      abi: callFromAbi,
      functionName: 'callFrom',
      args: [sender.account.address, receiver.account.address, '0x'],
    })

    await expect(
      sender.sendTransaction({
        to: callFromAddress,
        data: callFromData,
      }),
    ).to.be.rejectedWith('unauthorized caller')
  })

  // Non-allowlisted contract (CallHelper) calling callFrom directly is rejected.
  // sender → CallHelper.execute → callFrom(CallHelper, receiver, "0x") → "unauthorized caller"
  it('non-allowlisted contract calling callFrom directly is rejected', async () => {
    const { client, sender, receiver } = await clients()

    const callFromData = encodeFunctionData({
      abi: callFromAbi,
      functionName: 'callFrom',
      args: [callHelper.address, receiver.account.address, '0x'],
    })

    const balances = await balancesSnapshot(client, {
      sender,
      callHelper: callHelper.address,
    })

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.execute([callFromAddress, callFromData, 0n])
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectExecutionResult({
        helper: callHelper.address,
        success: false,
        revertString: 'unauthorized caller',
      }).expectAllEventsMatched()
    })

    // No balance leaked
    await balances.decrease({ sender: receipt.totalFee() }).verify()
  })
})

describe('Memo + Multicall3From', () => {
  // Each transfer individually wrapped with memo inside a batch.
  // sender → Multicall3From.aggregate3 → callFrom(sender, Memo, memo(USDC, transfer)) × 2
  it('per-call memos inside batch', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const startIdx = await readMemoIndex()
    const totalTransferred = USDC.toNative(amt1 + amt2)

    const balances = await balancesSnapshot(client, { sender, receiver, totalSupply })

    const transfer1 = encodeUSDCTransfer(receiver.account.address, amt1)
    const transfer2 = encodeUSDCTransfer(receiver.account.address, amt2)

    const callData = encodeAggregate3([
      {
        target: memoAddress,
        allowFailure: false,
        callData: encodeMemo(USDC.address, transfer1, memoId1, memo1),
      },
      {
        target: memoAddress,
        allowFailure: false,
        callData: encodeMemo(USDC.address, transfer2, memoId2, memo2),
      },
    ])

    const receipt = await sender
      .sendTransaction({ to: multicall3FromAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      // First memo
      ev.expectBeforeMemo({ memoIndex: startIdx })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt1) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt1 })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash: keccak256(transfer1),
          memoId: memoId1,
          memo: memo1,
          memoIndex: startIdx,
        })
        // Second memo
        .expectBeforeMemo({ memoIndex: startIdx + 1n })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt2) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt2 })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash: keccak256(transfer2),
          memoId: memoId2,
          memo: memo2,
          memoIndex: startIdx + 1n,
        })
        .expectAllEventsMatched()
    })

    const endIdx = await readMemoIndex()
    expect(endIdx).to.eq(startIdx + 2n)

    await balances
      .decrease({ sender: totalTransferred + receipt.totalFee() })
      .increase({ receiver: totalTransferred })
      .verify()
  })

  // Single memo wrapping an entire batch — Memo target is Multicall3From, not individual USDC calls.
  // sender → Memo(Multicall3From, aggregate3) → callFrom × 2 → USDC.transfer × 2
  it('one memo covering entire batch', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const startIdx = await readMemoIndex()
    const totalTransferred = USDC.toNative(amt1 + amt2)

    const balances = await balancesSnapshot(client, { sender, receiver, totalSupply })

    const batchCallData = encodeAggregate3([
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt1) },
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt2) },
    ])

    const callData = encodeMemo(multicall3FromAddress, batchCallData, memoId1, memo1)

    const receipt = await sender
      .sendTransaction({ to: memoAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      ev.expectBeforeMemo({ memoIndex: startIdx })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt1) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt1 })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt2) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt2 })
        .expectMemo({
          sender,
          target: multicall3FromAddress,
          callDataHash: keccak256(batchCallData),
          memoId: memoId1,
          memo: memo1,
          memoIndex: startIdx,
        })
        .expectAllEventsMatched()
    })

    const endIdx = await readMemoIndex()
    expect(endIdx).to.eq(startIdx + 1n)

    await balances
      .decrease({ sender: totalTransferred + receipt.totalFee() })
      .increase({ receiver: totalTransferred })
      .verify()
  })

  // Outer batch containing a memo-wrapped inner batch and a memo-wrapped single call.
  // sender → Multicall3From.aggregate3([memo(batch), memo(transfer)])
  it('mixed nested composition', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const startIdx = await readMemoIndex()
    const totalTransferred = USDC.toNative(amt1 + amt2 + amt3)

    const balances = await balancesSnapshot(client, { sender, receiver, totalSupply })

    const innerBatch = encodeAggregate3([
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt1) },
      { target: USDC.address, allowFailure: false, callData: encodeUSDCTransfer(receiver.account.address, amt2) },
    ])

    const transfer3 = encodeUSDCTransfer(receiver.account.address, amt3)

    const callData = encodeAggregate3([
      {
        target: memoAddress,
        allowFailure: false,
        callData: encodeMemo(multicall3FromAddress, innerBatch, memoId1, memo1),
      },
      {
        target: memoAddress,
        allowFailure: false,
        callData: encodeMemo(USDC.address, transfer3, memoId2, memo2),
      },
    ])

    const receipt = await sender
      .sendTransaction({ to: multicall3FromAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      // First call: memo wrapping inner batch
      ev.expectBeforeMemo({ memoIndex: startIdx })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt1) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt1 })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt2) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt2 })
        .expectMemo({
          sender,
          target: multicall3FromAddress,
          callDataHash: keccak256(innerBatch),
          memoId: memoId1,
          memo: memo1,
          memoIndex: startIdx,
        })
        // Second call: memo wrapping single transfer
        .expectBeforeMemo({ memoIndex: startIdx + 1n })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(amt3) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: amt3 })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash: keccak256(transfer3),
          memoId: memoId2,
          memo: memo2,
          memoIndex: startIdx + 1n,
        })
        .expectAllEventsMatched()
    })

    const endIdx = await readMemoIndex()
    expect(endIdx).to.eq(startIdx + 2n)

    await balances
      .decrease({ sender: totalTransferred + receipt.totalFee() })
      .increase({ receiver: totalTransferred })
      .verify()
  })

  // First call reverts (insufficient balance, allowFailure=true), second succeeds.
  // Journal rollback undoes first call's memoIndex++ and events; memoIndex += 1 (not 2).
  it('revert propagation with allowFailure', async () => {
    const { client, sender, receiver, totalSupply } = await clients()
    const startIdx = await readMemoIndex()
    const smallAmt = amt1
    const totalTransferred = USDC.toNative(smallAmt)

    const balances = await balancesSnapshot(client, { sender, receiver, totalSupply })

    const failTransfer = encodeUSDCTransfer(receiver.account.address, maxUint256)
    const okTransfer = encodeUSDCTransfer(receiver.account.address, smallAmt)

    const callData = encodeAggregate3([
      {
        target: memoAddress,
        allowFailure: true,
        callData: encodeMemo(USDC.address, failTransfer, memoId1, memo1),
      },
      {
        target: memoAddress,
        allowFailure: false,
        callData: encodeMemo(USDC.address, okTransfer, memoId2, memo2),
      },
    ])

    const receipt = await sender
      .sendTransaction({ to: multicall3FromAddress, data: callData })
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      // Only events from the second (successful) call.
      // First call reverted — its BeforeMemo, memoIndex++, and any events are rolled back.
      ev.expectBeforeMemo({ memoIndex: startIdx })
        .expectNativeTransfer({ from: sender, to: receiver, amount: USDC.toNative(smallAmt) })
        .expectUSDCTransfer({ from: sender, to: receiver, value: smallAmt })
        .expectMemo({
          sender,
          target: USDC.address,
          callDataHash: keccak256(okTransfer),
          memoId: memoId2,
          memo: memo2,
          memoIndex: startIdx,
        })
        .expectAllEventsMatched()
    })

    const endIdx = await readMemoIndex()
    expect(endIdx).to.eq(startIdx + 1n)

    await balances
      .decrease({ sender: totalTransferred + receipt.totalFee() })
      .increase({ receiver: totalTransferred })
      .verify()
  })

  // Two sequential memos in a single tx via executeBatch — tests callFrom depth-keyed
  // continuation storage doesn't collide; memoIndex increments correctly across both.
  // sender → CallHelper.executeBatch → Memo × 2 → callFrom(CallHelper, USDC, transfer) × 2
  it('batch sequential memo in one transaction', async () => {
    const { client, sender, receiver } = await clients()

    const memoIndex = await readMemoIndex()
    const amount1 = USDC.parseUnits('0.0001')
    const amount2 = USDC.parseUnits('0.0002')
    const nativeAmount1 = USDC.toNative(amount1)
    const nativeAmount2 = USDC.toNative(amount2)
    const batchMemoId1 = keccak256(toHex('batch-1'))
    const batchMemoId2 = keccak256(toHex('batch-2'))
    const batchMemo1 = toHex('first')
    const batchMemo2 = toHex('second')

    const transferData1 = encodeUSDCTransfer(receiver.account.address, amount1)
    const transferData2 = encodeUSDCTransfer(receiver.account.address, amount2)
    const memoData1 = encodeMemo(USDC.address, transferData1, batchMemoId1, batchMemo1)
    const memoData2 = encodeMemo(USDC.address, transferData2, batchMemoId2, batchMemo2)

    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      callHelper: callHelper.address,
    })

    const receipt = await CallHelper.attach({ wallet: sender, public: client }, callHelper.address)
      .write.executeBatch([
        [
          { target: memoAddress, allowFailure: false, value: 0n, callData: memoData1 },
          { target: memoAddress, allowFailure: false, value: 0n, callData: memoData2 },
        ],
      ])
      .then(ReceiptVerifier.waitSuccess)

    receipt.verifyEvents((ev) => {
      // First call
      ev.expectBeforeMemo({ memoIndex })
        .expectNativeTransfer({ from: callHelper.address, to: receiver, amount: nativeAmount1 })
        .expectUSDCTransfer({ from: callHelper.address, to: receiver, value: amount1 })
        .expectMemo({
          sender: callHelper.address,
          target: USDC.address,
          callDataHash: keccak256(transferData1),
          memoId: batchMemoId1,
          memo: batchMemo1,
          memoIndex,
        })
        .expectExecutionResult({ helper: callHelper.address, success: true, result: '0x' })
        // Second call
        .expectBeforeMemo({ memoIndex: memoIndex + 1n })
        .expectNativeTransfer({ from: callHelper.address, to: receiver, amount: nativeAmount2 })
        .expectUSDCTransfer({ from: callHelper.address, to: receiver, value: amount2 })
        .expectMemo({
          sender: callHelper.address,
          target: USDC.address,
          callDataHash: keccak256(transferData2),
          memoId: batchMemoId2,
          memo: batchMemo2,
          memoIndex: memoIndex + 1n,
        })
        .expectExecutionResult({ helper: callHelper.address, success: true, result: '0x' })
        .expectAllEventsMatched()
    })

    // Both transfers from callHelper
    const totalNative = nativeAmount1 + nativeAmount2
    await balances
      .increase({ receiver: totalNative })
      .decrease({ callHelper: totalNative, sender: receipt.totalFee() })
      .verify()
  })
})
