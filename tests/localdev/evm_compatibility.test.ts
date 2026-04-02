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
import hre from 'hardhat'
import {
  Abi,
  Address,
  ContractFunctionExecutionError,
  decodeFunctionResult,
  encodeDeployData,
  encodeFunctionData,
  EstimateGasExecutionError,
  getContract,
  getCreate2Address,
  keccak256,
  pad,
  parseEther,
  toHex,
  TransactionExecutionError,
  zeroAddress,
} from 'viem'
import {
  balancesSnapshot,
  EventsVerifier,
  ReceiptVerifier,
  getClients,
  expectAddressEq,
  skipCompare,
  CallFrame,
  isArcNetwork,
  expectGasUSedApproximately,
  getNextBaseFee,
} from '../helpers'
import { CallHelper, CallInput } from '../helpers/CallHelper'
import { debugTraceFunctions, ExpectFrameResult, TraceFrameVerifier } from '../helpers'
import { addressToBytes32, toBytes32 } from '../../scripts/genesis/types'
import { NativeCoinControl } from '../helpers/NativeCoinControl'
import { Hex } from 'viem'
import { USDC } from '../helpers/FiatToken'

const errSelfDestructBalance = 'Cannot increase the balance of selfdestructed account'

describe('EVM compatibility', () => {
  const initClients = async () => {
    let seq = 0n
    const randSlot = () =>
      BigInt(new Date().getTime()) * 100000n + BigInt(Math.floor(Math.random() * 1000)) * 100n + seq++

    const { client, admin, sender, receiver } = await getClients()

    // three helper addresses A, B, C
    const amount = parseEther('0.1')
    const A = await CallHelper.deterministicDeploy(sender, client, amount, 0n).then((x) => x.address)
    const B = await CallHelper.deterministicDeploy(sender, client, amount, 1n).then((x) => x.address)
    const C = await CallHelper.deterministicDeploy(sender, client, amount, 2n).then((x) => x.address)

    return { client: client.extend(debugTraceFunctions), admin, sender, receiver, A, B, C, randSlot }
  }

  let clients: Awaited<ReturnType<typeof initClients>>
  before(async () => {
    clients = await initClients()
  })

  describe('block info', () => {
    function decodeBlockInfo(output: Hex) {
      return decodeFunctionResult({
        abi: CallHelper.abi,
        functionName: 'getBlockInfo',
        data: output ?? '0x',
      })
    }
    type Block = Awaited<ReturnType<typeof clients.client.getBlock<false, 'latest'>>>
    type BlockInfo = ReturnType<typeof decodeBlockInfo>
    function expectFutureBlock(
      previous: Block,
      blockInfo: BlockInfo,
      opts?: { overrideGasLimit?: true; baseFee?: bigint },
    ) {
      expect(Number(blockInfo.number)).to.be.gte(Number(previous.number))
      expect(Number(blockInfo.timestamp)).to.be.gte(Number(previous.timestamp))
      expect(blockInfo.coinbase).to.not.be.eq(zeroAddress)

      // The gas limit may change for EIP1559 so skip the check
      if (isArcNetwork()) {
        if (opts?.overrideGasLimit === true) {
          // For reth node, this is configured by `--rpc.gascap` default value is 30000000n
          // The setting should greater or equal than gas limit in the block
          expect(blockInfo.gasLimit).to.be.eq(30000000n)
        } else {
          // In arc, the gas limit follow the setting in the ProtocolConfig
          // It will not change during these tests
          expect(blockInfo.gasLimit).to.be.eq(previous.gasLimit)
        }
      }
      if (opts?.baseFee != null) {
        expect(blockInfo.baseFee).to.be.eq(opts.baseFee)
      }
      //expect(blockInfo.prevRandao).to.not.be.eq(0n)
    }
    function expectSameBlock(block: Block, blockInfo: BlockInfo) {
      expect(blockInfo.number).to.be.eq(block.number)
      expect(blockInfo.timestamp).to.be.eq(block.timestamp)
      expect(blockInfo.coinbase).to.be.addressEqual(block.miner)
      expect(blockInfo.baseFee).to.be.eq(block.baseFeePerGas)
      expect(blockInfo.gasLimit).to.be.eq(block.gasLimit)
      //expect(blockInfo.prevRandao).to.not.be.eq(0n)
    }

    it('read block info', async () => {
      const { client, A } = clients
      const latest = await client.getBlock({ includeTransactions: false })
      const blockInfo = await CallHelper.attach(client, A).read.getBlockInfo()
      expectFutureBlock(latest, blockInfo, {
        // For reth node, this is configured by `--rpc.gascap` default value is 30000000n
        // The setting should greater or equal than gas limit in the block
        overrideGasLimit: true,
        // For eth_call, the base fee should be zero
        baseFee: 0n,
      })
    })

    // This test could be only verified if auto-mine is disabled
    it.skip('read block info with maxFeePerGas', async () => {
      const { client, A } = clients
      const { block: latest, next: pending } = await getNextBaseFee(client)

      const res = await client.request({
        method: 'eth_call',
        params: [
          {
            to: A,
            data: CallHelper.encodeNested({ fn: 'getBlockInfo' }),
            maxFeePerGas: toHex(pending.baseFeePerGas),
          },
          'pending',
        ],
      })
      const blockInfo = decodeBlockInfo(res)
      expectFutureBlock(latest, blockInfo, {
        overrideGasLimit: true,
        baseFee: pending.baseFeePerGas,
      })
    })

    it('block info in transaction', async () => {
      const { client, sender, A } = clients
      const previous = await client.getBlock({ includeTransactions: false })
      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'execute',
            target: A,
            data: { fn: 'getBlockInfo' },
          }),
        })
        .then(ReceiptVerifier.wait)
      const block = await client.getBlock({ blockHash: receipt.blockHash })

      receipt.verifyEvents((ev) => {
        const blockInfo = decodeBlockInfo(ev.getExecutionResult(0))
        expectFutureBlock(previous, blockInfo)
        expectSameBlock(block, blockInfo)
      })

      // check the block info in trace transaction
      const res = await client
        .extend(debugTraceFunctions)
        .traceTransaction(receipt.transactionHash, { tracer: 'callTracer' })

      const output = res.calls?.[0].output
      expect(output).to.not.be.undefined
      expectSameBlock(block, decodeBlockInfo(output ?? '0x'))
    })

    if (isArcNetwork()) {
      // geth: tracing on top of pending is not supported
      it('block info in pending trace', async () => {
        const { client, A } = clients
        const latest = await client.getBlock({ includeTransactions: false })
        const res = await client.extend(debugTraceFunctions).traceCall(
          {
            to: A,
            data: CallHelper.encodeNested({ fn: 'getBlockInfo' }),
          },
          'pending',
          { tracer: 'callTracer' },
        )

        const blockInfo = decodeBlockInfo(res.output ?? '0x')
        expectFutureBlock(latest, blockInfo, { overrideGasLimit: true, baseFee: 0n })
      })
    }

    it('block info in simulation calls', async () => {
      const { sender, client, A } = clients
      const latest = await client.getBlock({ includeTransactions: false })
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, data: CallHelper.encodeNested({ fn: 'getBlockInfo' }) }],
      })
      expect(res.results[0].status).to.be.eq('success')
      expectFutureBlock(latest, decodeBlockInfo(res.results[0].data), { baseFee: 0n })
    })

    it('block info in simulate blocks', async () => {
      const { sender, client, A } = clients
      const overrideBaseFee = 8817n
      const latest = await client.getBlock({ includeTransactions: false })
      const res = await client.simulateBlocks({
        blocks: [
          {
            blockOverrides: { baseFeePerGas: overrideBaseFee },
            calls: [{ account: sender.account.address, to: A, data: CallHelper.encodeNested({ fn: 'getBlockInfo' }) }],
          },
        ],
      })
      expect(res[0].calls[0].status).to.be.eq('success')
      expectFutureBlock(latest, decodeBlockInfo(res[0].calls[0].data), { baseFee: overrideBaseFee })
    })
  })

  describe('error handling', () => {
    it('reverted message', async () => {
      const { sender, client, A } = clients
      const balances = await balancesSnapshot(client, { sender, A })
      await expect(
        sender.sendTransaction({
          to: A,
          data: CallHelper.encodeNested({ fn: 'revertWithString', message: 'test' }),
          value: parseEther('0.01'),
        }),
      ).to.be.rejectedWith(TransactionExecutionError, 'test')
      await balances.verify()
    })

    it('send raw transaction reverted', async () => {
      const { sender, client, A } = clients
      const balances = await balancesSnapshot(client, { sender, A })
      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({ fn: 'revertWithString', message: 'test' }),
          value: parseEther('0.01'),
          gas: 1000000n, // skip estimation
        })
        .then(ReceiptVerifier.wait)
      receipt.isReverted()
      await balances.decrease({ sender: receipt.totalFee() }).verify()
    })

    it('reverted error object', async () => {
      const { sender, A } = clients
      await expect(
        sender.sendTransaction({
          to: A,
          data: CallHelper.encodeNested({ fn: 'revertWithError', message: 'test error' }),
          value: parseEther('0.01'),
        }),
      ).to.be.rejectedWith(TransactionExecutionError, 'unknown reason')
    })

    it('error object should be parsed', async () => {
      const { sender, A } = clients
      await expect(CallHelper.attach(sender, A).write.revertWithError(['some error'])).to.be.rejectedWith(
        ContractFunctionExecutionError,
        /ErrorMessage(.|\r|\n)*some error/,
      )
    })

    it('catch indirect transfer reverted', async () => {
      const { client, sender, A } = clients
      const amount = parseEther('0.0003')
      const balances = await balancesSnapshot(client, { sender, A })
      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'execute',
            target: A,
            data: { fn: 'revertWithError', message: 'test' },
            value: parseEther('0.00004'),
          }),
          value: amount,
        })
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount })
          .expectExecutionResult({ helper: A, success: false, revertError: 'test' })
          .expectAllEventsMatched()
      })
      await balances
        .decrease({ sender: receipt.totalFee() + amount })
        .increase({ A: amount })
        .verify()
    })

    it('internal revert message', async () => {
      const { sender, A, randSlot } = clients
      const slot = randSlot()
      const amount1 = parseEther('0.000006')
      const amount2 = parseEther('0.000005')
      const amount3 = parseEther('0.000004')
      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              { target: A, allowFailure: true, data: { fn: 'getStorage', slot } },
              { target: A, allowFailure: true, data: { fn: 'revertWithString', message: 'wrong 3' }, value: amount2 },
              { target: A, value: amount3 },
              { target: A, allowFailure: true, data: { fn: 'revertWithError', message: 'wrong 4' } },
            ],
          }),
          value: amount1,
        })
        .then(ReceiptVerifier.waitSuccess)

      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          .expectExecutionResult({ helper: A, fn: 'getStorage', value: 0n })
          .expectExecutionResult({ helper: A, success: false, revertString: 'wrong 3' })
          // EIP-7708: self-transfer (A → A) emits no Transfer log
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: false, revertError: 'wrong 4' })
          .expectAllEventsMatched()
      })
    })
  })

  describe('different call context', () => {
    const testSetStorage = async (fn: 'execute' | 'callCode' | 'delegateCall' | 'staticCall', value: bigint) => {
      const { sender, A, B, C, randSlot } = clients
      const slot = randSlot()
      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              { target: B, data: { fn, target: C, data: { fn: 'setStorage', slot, value } }, allowFailure: true },
              { target: A, data: { fn: 'getStorage', slot } },
              { target: B, data: { fn: 'getStorage', slot } },
              { target: C, data: { fn: 'getStorage', slot } },
            ],
          }),
        })
        .then(ReceiptVerifier.waitSuccess)

      const verifyChange = (ev: EventsVerifier, expectChangeAddress?: Address) => {
        for (const addr of [A, B, C]) {
          ev.expectExecutionResult({
            helper: A,
            fn: 'getStorage',
            value: expectChangeAddress === addr ? value : 0n,
          })
        }
        ev.expectAllEventsMatched()
      }
      return { receipt, A, B, C, slot, value, verifyChange }
    }
    it('execute setStorage', async () => {
      const { receipt, A, B, C, slot, value, verifyChange } = await testSetStorage('execute', 13n)
      receipt.verifyEvents((ev) => {
        ev.expectCallHelperStorageSet({ helper: C, sender: B, slot, value })
          .expectExecutionResult({ helper: B, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, nested: { success: true, result: '0x' } })
        verifyChange(ev, C)
      })
    })
    it('staticCall setStorage', async () => {
      const { receipt, A, B, verifyChange } = await testSetStorage('staticCall', 61n)
      receipt.verifyEvents((ev) => {
        ev.expectExecutionResult({ helper: B, success: false, result: '0x' }).expectExecutionResult({
          helper: A,
          success: true,
          nested: { success: false, result: '0x' },
        })
        verifyChange(ev, undefined)
      })
    })
    it('delegateCall setStorage', async () => {
      const { receipt, A, B, slot, value, verifyChange } = await testSetStorage('delegateCall', 79n)
      receipt.verifyEvents((ev) => {
        ev.expectCallHelperStorageSet({ helper: B, sender: A, slot, value })
          .expectExecutionResult({ helper: B, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, nested: { success: true, result: '0x' } })
        verifyChange(ev, B)
      })
    })
    it('callCall setStorage', async () => {
      const { receipt, A, B, slot, value, verifyChange } = await testSetStorage('callCode', 43n)
      receipt.verifyEvents((ev) => {
        ev.expectCallHelperStorageSet({ helper: B, sender: B, slot, value })
          .expectExecutionResult({ helper: B, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, nested: { success: true, result: '0x' } })
        verifyChange(ev, B)
      })
    })

    // Helper: Deploy PrecompileDelegater contract
    const deployPrecompileDelegater = async () => {
      const { client, sender } = clients
      const artifact = await hre.artifacts.readArtifact('PrecompileDelegater')
      const deployHash = await sender.deployContract({
        abi: artifact.abi,
        bytecode: artifact.bytecode as Hex,
        args: [],
      })

      const deployReceipt = await client.waitForTransactionReceipt({ hash: deployHash })
      expect(deployReceipt.status).to.equal('success')
      expect(deployReceipt.contractAddress).to.exist

      const address = deployReceipt.contractAddress as Address
      const contract = getContract({ abi: artifact.abi as Abi, address, client: { public: client, wallet: sender } })

      return { address, contract, artifact }
    }

    it('USDC.permit(staticCall) -> PrecompileDelegater.isValidSignature(delegateCall) -> NativeCoinAuthority.mint should block minting', async () => {
      const { client, sender } = clients

      const { address: maliciousAddress, contract: malicious } = await deployPrecompileDelegater()

      // Get the contract owner (who would receive minted tokens)
      const owner = (await malicious.read.owner()) as Address
      const initialOwnerBalance = await client.getBalance({ address: owner })

      // Prepare permit parameters
      const spender = sender.account.address
      const value = 1000000n
      const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600)
      const dummySignature = pad('0x00', { size: 65 })

      // Call permit which triggers isValidSignature() -> delegatecall(NativeCoinAuthority.mint)
      // Permit uses STATICCALL per EIP-1271, which invokes the delegatecall on PrecompileDelegater.isValidSignature()
      // The delegatecall failure causes isValidSignature to revert, which causes permit to fail
      const permitPromise = USDC.attach(sender)
        .write.permit([maliciousAddress, spender, value, deadline, dummySignature])
        .then((hash) => client.waitForTransactionReceipt({ hash }))

      // Permit should fail because the delegatecall is blocked and error bubbles up
      await expect(permitPromise).to.be.rejectedWith(ContractFunctionExecutionError, /EIP2612: invalid signature/)

      // Verify no tokens were minted
      const finalOwnerBalance = await client.getBalance({ address: owner })
      const balanceIncrease = finalOwnerBalance - initialOwnerBalance
      expect(balanceIncrease).to.lessThanOrEqual(0n, 'No tokens should be minted')
    })

    it('USDC.rescueERC20(PrecompileDelegater.transfer) -> PrecompileDelegater.transfer(delegateCall) -> NativeCoinAuthority.mint should fail to mint tokens', async () => {
      const { client, admin } = clients

      const { address: maliciousAddress, contract: malicious } = await deployPrecompileDelegater()

      // Get the contract owner (who would receive minted tokens)
      const owner = (await malicious.read.owner()) as Address
      const initialOwnerBalance = await client.getBalance({ address: owner })

      // Attempt rescueERC20 which will call malicious.transfer() -> delegatecall(NativeCoinAuthority.mint)
      const rescueAmount = parseEther('98765')
      const txPromise = USDC.attach(admin)
        .write.rescueERC20([maliciousAddress, owner, rescueAmount])
        .then((hash) => client.waitForTransactionReceipt({ hash }))

      // Transaction should fail because the malicious transfer() returns false (delegatecall to precompile fails)
      await expect(txPromise).to.be.rejectedWith(ContractFunctionExecutionError, /Delegate call not allowed/)

      // Verify attack was not successful
      const finalOwnerBalance = await client.getBalance({ address: owner })
      const balanceIncrease = finalOwnerBalance - initialOwnerBalance
      expect(balanceIncrease).to.lessThanOrEqual(0n, 'No tokens should be minted')
    })

    it('USDC.rescueERC20 -> PrecompileCallCode.transfer(callCode) -> NativeCoinAuthority.mint should fail', async () => {
      const { client, sender, admin } = clients

      // Deploy PrecompileCallCode
      const artifact = await hre.artifacts.readArtifact('PrecompileCallCode')
      const deployHash = await sender.deployContract({
        abi: artifact.abi,
        bytecode: artifact.bytecode as Hex,
        args: [],
      })

      const deployReceipt = await client.waitForTransactionReceipt({ hash: deployHash })
      expect(deployReceipt.status).to.equal('success')
      expect(deployReceipt.contractAddress).to.exist

      const contractAddress = deployReceipt.contractAddress!
      const contract = getContract({
        abi: artifact.abi as Abi,
        address: contractAddress,
        client: { public: client, wallet: sender },
      })

      // Get the contract owner (who would receive minted tokens)
      const owner = (await contract.read.owner()) as Address
      const initialOwnerBalance = await client.getBalance({ address: owner })

      // Attempt rescueERC20 which will call contract.transfer() -> callCode(NativeCoinAuthority.mint)
      const rescueAmount = parseEther('98765')
      const txPromise = USDC.attach(admin)
        .write.rescueERC20([contractAddress, owner, rescueAmount])
        .then((hash) => client.waitForTransactionReceipt({ hash }))

      // Transaction should fail because callCode to precompile doesn't work properly
      await expect(txPromise).to.be.rejectedWith(ContractFunctionExecutionError, /Not enabled native coin minter/)

      // Verify no tokens were minted
      const finalOwnerBalance = await client.getBalance({ address: owner })
      const balanceIncrease = finalOwnerBalance - initialOwnerBalance
      expect(balanceIncrease).to.lessThanOrEqual(0n, 'No tokens should be minted - callCode not supported')
    })

    /**
     * A chain test case. Sender create the transaction to call A.fn, where
     * fn is one of 'execute', 'callCode', 'delegateCall', 'staticCall'.
     * then call B.execute() to transfer native token to C.
     *
     * sender {amount1} -> A.('execute' | 'callCode' | 'delegateCall' | 'staticCall')
     *   ? {amount2} -> B.execute()
     *     ? {amount3} -> C
     */
    const testNativeTransfer = async (fn: 'execute' | 'callCode' | 'delegateCall' | 'staticCall', gas?: bigint) => {
      const { client, sender, A, B, C } = clients
      const amount1 = parseEther('0.000001')
      const amount2 = parseEther('0.000002')
      const amount3 = parseEther('0.000003')
      const balances = await balancesSnapshot(client, { sender, A, B, C })

      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn,
            target: B,
            data: { fn: 'transfer', to: C, value: amount3 },
            value: amount2,
          }),
          value: amount1,
          gas,
        })
        .then(ReceiptVerifier.waitSuccess)

      return { receipt, balances, amount1, amount2, amount3, A, B, C, sender }
    }
    it('execute transfer native', async () => {
      const { receipt, balances, sender, A, B, C, amount1, amount2, amount3 } = await testNativeTransfer('execute')
      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          .expectNativeTransfer({ from: A, to: B, amount: amount2 })
          .expectNativeTransfer({ from: B, to: C, amount: amount3 })
          .expectExecutionContext({ helper: B, sender: A, value: amount2 })
          .expectExecutionResult({ helper: B, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
      })
      // Sender transfers to A, A transfers to B, B transfers to C.
      await balances
        .decrease({ sender: receipt.totalFee() + amount1, A: amount2, B: amount3 })
        .increase({ A: amount1, B: amount2, C: amount3 })
        .verify(receipt.transactionHash)
    })
    it('staticCall transfer native', async () => {
      // geth
      //      gas used 11,694,360 (use estimated gas limit)
      //      gas used    396,389 (limit 400000)
      //      gas used    297,952 (limit 300000)
      //      gas used    199,514 (limit 200000)
      //      gas used    168,890 (limit 168889)
      //      oog (limit 168888n)
      // arc/reth
      //      gas used 29,447,816 (use estimated gas limit)
      //      gas used    396,389 (limit 400000)
      //      gas used    297,952 (limit 300000)
      //      gas used    199,514 (limit 200000)
      //      gas used    168,890 (limit 168889)
      //      oog (limit 168888n)
      //
      // For static call revert, the gas usage depends on the gas limit.
      // So use the specified gas here to get the consistent result.
      const { receipt, balances, sender, A, amount1 } = await testNativeTransfer('staticCall', 200000n)
      receipt.verifyEvents((ev) => {
        // Can not transfer value in static call context. execution reverted without error message.
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          .expectExecutionResult({ helper: A, success: false, result: '0x' })
          .expectAllEventsMatched()
      })
      // Only sender transfers to A.
      await balances
        .decrease({ sender: receipt.totalFee() + amount1 })
        .increase({ A: amount1 })
        .verify(receipt.transactionHash)

      expect(Number(receipt.gasUsed)).to.be.approximately(199514, 1000)
    })
    it('delegateCall transfer native', async () => {
      const { receipt, balances, A, C, sender, amount1, amount3 } = await testNativeTransfer('delegateCall')
      receipt.verifyEvents((ev) => {
        // Both context are from sender, since the A.delegateCall is executed by sender.
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          .expectNativeTransfer({ from: A, to: C, amount: amount3 })
          .expectExecutionContext({ helper: A, sender, value: amount1 })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectAllEventsMatched()
      })
      // Sender transfers to A then A transfers to C.
      await balances
        .decrease({ sender: receipt.totalFee() + amount1, A: amount3 })
        .increase({ A: amount1, C: amount3 })
        .verify(receipt.transactionHash)
    })
    it('callCode transfer native', async () => {
      const { receipt, balances, sender, A, C, amount1, amount2, amount3 } = await testNativeTransfer('callCode')
      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          .expectNativeTransfer({ from: A, to: C, amount: amount3 })
          .expectExecutionContext({ helper: A, sender: A, value: amount2 })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectAllEventsMatched()
      })
      // Almost the same as execute case.
      // Sender transfers to A, A transfers to C.
      await balances
        .decrease({ sender: receipt.totalFee() + amount1, A: amount3 })
        .increase({ A: amount1, C: amount3 })
        .verify(receipt.transactionHash)
    })
  })

  describe('debug trace', () => {
    it('trace simple transfer', async () => {
      const { client, sender, receiver } = clients
      const amount = parseEther('0.000001')

      const receipt = await sender
        .sendTransaction({ to: receiver.account.address, value: amount })
        .then(ReceiptVerifier.waitSuccess)
      const tx = await client.getTransaction({ hash: receipt.transactionHash })
      const block = await client.getBlock({ blockHash: receipt.blockHash, includeTransactions: false })
      const expectResult: ExpectFrameResult = {
        from: sender,
        to: receiver,
        value: amount,
        type: 'CALL',
        gasUsed: receipt.gasUsed,
        gas: tx.gas,
        input: '0x',
        output: skipCompare,
        error: undefined,
        revertReason: undefined,
      }

      // check debug_traceTransaction result with callTracer
      const frame = await client
        .traceTransaction(receipt.transactionHash, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then((x) => TraceFrameVerifier.frame(x, receipt.transactionHash))
      frame.expectMatch(expectResult).verifyEvents((_ev) => {
        // expect(res.logs).to.be.an('array').with.lengthOf(1)
        // ev.expectNativeTransfer({ from: sender, to: receiver, amount })
      })

      // check debug_traceBlockByHash result with callTracer
      const blockByHash = await client
        .traceBlockByHash(receipt.blockHash, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then(TraceFrameVerifier.blockFrames)
      expect(blockByHash).to.be.an('array').with.lengthOf(1)
      blockByHash[0].expectMatch(expectResult)

      // check debug_traceBlockByNumber result with callTracer
      const blockByNumber = await client
        .traceBlockByNumber(receipt.blockNumber, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then(TraceFrameVerifier.blockFrames)
      expect(blockByNumber).to.be.an('array').with.lengthOf(1)
      blockByHash[0].expectMatch(expectResult)

      // check debug_traceTransaction result with prestateTracer
      const storage = await client.traceTransaction(receipt.transactionHash, {
        tracer: 'prestateTracer',
        tracerConfig: { diffMode: false, disableCode: true, disableStorage: false },
      })
      if (isArcNetwork()) {
        expect(storage).to.have.property(NativeCoinControl.address).to.have.property('storage')
      }
      expect(storage).to.have.property(sender.account.address).to.have.property('nonce')
      expect(storage).to.have.property(sender.account.address).to.have.property('balance')
      expect(storage).to.have.property(receiver.account.address).to.have.property('balance')
      expect(storage).to.have.property(block.miner).to.have.property('balance')

      // check debug_traceTransaction result with prestateTracer diff mode
      const storageDiff = await client.traceTransaction(receipt.transactionHash, {
        tracer: 'prestateTracer',
        tracerConfig: { diffMode: true, disableCode: true, disableStorage: false },
      })
      expect(storageDiff).to.have.property('pre')
      expect(storageDiff.pre).to.have.property(sender.account.address).to.have.property('balance')
      expect(storageDiff).to.have.property('post')
      expect(storageDiff.post).to.have.property(sender.account.address).to.have.property('nonce')
      expect(storageDiff.post).to.have.property(receiver.account.address).to.have.property('balance')
      expect(storageDiff.post).to.have.property(block.miner).to.have.property('balance')

      // check debug_traceBlockByHash result with prestateTracer
      const blockStorage = await client.traceBlockByHash(receipt.blockHash, {
        tracer: 'prestateTracer',
        tracerConfig: { diffMode: false, disableCode: true, disableStorage: false },
      })
      expect(blockStorage).to.be.an('array').with.lengthOf(1)
      expect(blockStorage[0].result).to.have.property(receiver.account.address).to.have.property('balance')
      expect(blockStorage[0].result).to.have.property(sender.account.address).to.have.property('balance')

      // check debug_traceBlockByNumber result with prestateTracer diff mode
      const blockStorageDiff = await client.traceBlockByNumber(receipt.blockNumber, {
        tracer: 'prestateTracer',
        tracerConfig: { diffMode: true, disableCode: true, disableStorage: false },
      })
      expect(blockStorage).to.be.an('array').with.lengthOf(1)
      expect(blockStorageDiff[0].result).to.have.property('pre')
      expect(blockStorageDiff[0].result.pre).to.have.property(sender.account.address).to.have.property('balance')
      expect(blockStorageDiff[0].result).to.have.property('post')
      expect(blockStorageDiff[0].result.post).to.have.property(sender.account.address).to.have.property('nonce')
      expect(blockStorageDiff[0].result.post).to.have.property(receiver.account.address).to.have.property('balance')
    })

    it('trace nested calls', async () => {
      const { client, sender, A, B, C, randSlot } = clients
      const slot = randSlot()
      const amount = parseEther('0.000001')
      const params: CallInput = {
        fn: 'executeBatch',
        calls: [
          { target: B, value: amount },
          { target: B, data: { fn: 'staticCall', target: C, data: { fn: 'getStorage', slot } } },
          {
            target: B,
            data: { fn: 'delegateCall', target: C, data: { fn: 'setStorage', slot, value: 3n } },
          },
        ],
      }

      const receipt = await sender
        .sendTransaction({ to: A, data: CallHelper.encodeNested(params) })
        .then(ReceiptVerifier.waitSuccess)

      const tx = await client.getTransaction({ hash: receipt.transactionHash })
      const verifyResult = (frame: TraceFrameVerifier & CallFrame) => {
        const expectResult: ExpectFrameResult = {
          from: sender,
          to: A,
          value: 0n,
          type: 'CALL',
          gasUsed: receipt.gasUsed,
          gas: tx.gas,
          input: CallHelper.encodeNested(params),
          output: CallHelper.result({
            fn: 'executeBatch',
            results: [
              { success: true, result: '0x' },
              { success: true, nested: { fn: 'getStorage', value: 0n } },
              { success: true, nested: { success: true, result: '0x' } },
            ],
          }).result,
          error: undefined,
          revertReason: undefined,
        }
        frame.expectMatch(expectResult)
        expect(frame.calls).to.be.an('array').with.lengthOf(3)
        // compare A -> B transfer
        frame.verifySubFrame(0, (frame) => {
          frame.expectPartialMatch({
            from: A,
            to: B,
            type: 'CALL',
            input: '0x',
            value: toHex(amount),
          })
        })

        // compare A -> B.staticCall -> C.getStorage
        frame.verifySubFrame(1, (frame) => {
          frame.expectPartialMatch({
            from: A,
            to: B,
            type: 'CALL',
            input: CallHelper.encodeNested(params.calls[1].data!),
          })
          expect(frame.logs).to.be.an('array').with.lengthOf(1)
          expect(frame.calls).to.be.an('array').with.lengthOf(1)

          frame.verifySubFrame(0, (frame) => {
            frame.expectPartialMatch({
              from: B,
              to: C,
              type: 'STATICCALL',
              input: CallHelper.encodeNested((params.calls[1].data as { data: CallInput }).data),
            })
          })
        })

        // compare A -> B.delegateCall -> C.setStorage
        frame.verifySubFrame(2, (frame) => {
          frame.expectPartialMatch({
            from: A,
            to: B,
            type: 'CALL',
            input: CallHelper.encodeNested(params.calls[2].data!),
          })
          expect(frame.calls).to.be.an('array').with.lengthOf(1)
          expect(frame.logs).to.be.an('array').with.lengthOf(1)

          frame.verifySubFrame(0, (frame) => {
            frame.expectPartialMatch({
              from: B,
              to: C,
              type: 'DELEGATECALL',
              input: CallHelper.encodeNested((params.calls[2].data as { data: CallInput }).data),
            })
          })
        })
      }

      const frame = await client
        .traceTransaction(receipt.transactionHash, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then((x) => TraceFrameVerifier.frame(x, receipt.transactionHash))
      verifyResult(frame)

      const blockByHash = await client
        .traceBlockByHash(receipt.blockHash, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then(TraceFrameVerifier.blockFrames)
      expect(blockByHash).to.be.an('array').with.lengthOf(1)
      verifyResult(blockByHash[0])

      const blockByNumber = await client
        .traceBlockByNumber(receipt.blockNumber, { tracer: 'callTracer', tracerConfig: { withLog: true } })
        .then(TraceFrameVerifier.blockFrames)
      expect(blockByNumber).to.be.an('array').with.lengthOf(1)
      verifyResult(blockByNumber[0])
    })

    it('traceCall with callTracer', async () => {
      const { client, sender, A } = clients
      const res = await client.traceCall(
        {
          to: A,
          from: sender.account.address,
          data: CallHelper.encodeNested({ fn: 'getBlockInfo' }),
        },
        'latest',
        { tracer: 'callTracer' },
      )
      expectAddressEq(res.from, sender)
      expectAddressEq(res.to, A)
      expect(res.gas).to.be.eq(toHex(50_000_000))
      expect(res.input).to.be.eq(CallHelper.encodeNested({ fn: 'getBlockInfo' }))
      expect(res.type).to.be.eq('CALL')
      expect(res.value).to.be.eq(toHex(0n))

      expect(res.output).to.be.not.undefined
      const blockInfo = decodeFunctionResult({
        abi: CallHelper.abi,
        functionName: 'getBlockInfo',
        data: res.output!,
      })
      expect(Number(blockInfo.number)).to.be.gt(0)
      expect(Number(blockInfo.timestamp)).to.be.gt(0)
      expect(blockInfo.coinbase).to.be.not.eq(zeroAddress)
    })

    it('traceCall with revert message', async () => {
      const { client, sender, A } = clients
      const res = await client.traceCall(
        {
          to: A,
          from: sender.account.address,
          data: CallHelper.encodeNested({ fn: 'revertWithString', message: 'contract message' }),
        },
        'latest',
        { tracer: 'callTracer' },
      )
      expect(res.error).to.be.eq('execution reverted')
      expect(res.revertReason).to.be.eq('contract message')
    })

    it('traceCall with revert error', async () => {
      const { client, sender, A } = clients
      const res = await client.traceCall(
        {
          to: A,
          from: sender.account.address,
          data: CallHelper.encodeNested({ fn: 'revertWithError', message: 'contract message' }),
        },
        'latest',
        { tracer: 'callTracer' },
      )
      expect(res.error).to.be.eq('execution reverted')
      expect(res.revertReason).to.be.undefined
      expect(res.output).to.be.eq(CallHelper.encodeError('contract message'))
    })

    it('traceCall with prestateTracer diff mode', async () => {
      const { client, sender, A, randSlot } = clients
      const slot = randSlot()
      const res = await client.traceCall(
        {
          to: A,
          from: sender.account.address,
          data: CallHelper.encodeNested({ fn: 'setStorage', slot, value: 48n }),
        },
        'latest',
        { tracer: 'prestateTracer', tracerConfig: { diffMode: true, disableCode: true } },
      )
      expect(res)
        .to.have.property('post')
        .to.have.property(A)
        .with.property('storage')
        .with.property(toBytes32(slot))
        .to.be.eq(toBytes32(48n))
    })

    it('traceCall with prestateTracer', async () => {
      const { client, sender, A, randSlot } = clients
      const slot = randSlot()
      const res = await client.traceCall(
        {
          to: A,
          from: sender.account.address,
          data: CallHelper.encodeNested({ fn: 'setStorage', slot, value: 51203n }),
        },
        'latest',
        { tracer: 'prestateTracer' },
      )
      expect(res).to.have.property(A).with.property('storage').with.property(toBytes32(slot)).to.be.eq(toBytes32(0n))
    })

    // Type definition for our JavaScript tracer response
    type OpcodeCountTracerResult = {
      opCount: Record<string, number>
    }

    // Helper function to create JavaScript tracer that counts opcodes
    const createOpcodeCountTracer = () => `
      (function(){
        var opCount = {};
        return {
          step: function(log, db) {
            var op = log.op.toString();
            if (opCount[op]) {
              opCount[op]++;
            } else {
              opCount[op] = 1;
            }
          },
          result: function() {
            return { opCount: opCount };
          },
          fault: function(log, db) {
            return { error: log.getError() };
          }
        }
      })()
    `

    // Helper function for common contract call data
    const getContractCallData = () => CallHelper.encodeNested({ fn: 'getBlockInfo' })

    // Helper function to verify JavaScript tracer results
    const verifyJsTracerResult = (res: OpcodeCountTracerResult) => {
      expect(res).to.have.property('opCount')
      expect(res.opCount).to.be.an('object')
      expect(Object.keys(res.opCount).length).to.be.greaterThan(0)

      // The getBlockInfo call should result in opcodes that read block properties.
      const hasBlockInfoOpcodes = Object.keys(res.opCount).some((op) =>
        ['NUMBER', 'TIMESTAMP', 'COINBASE', 'GASLIMIT'].includes(op),
      )
      expect(hasBlockInfoOpcodes, 'Expected tracer output to contain opcodes for reading block information').to.be.true
    }

    it('traceCall with JavaScript tracer', async () => {
      const { client, sender, A } = clients

      const res = await client.traceCall(
        {
          from: sender.account.address,
          to: A,
          data: getContractCallData(),
        },
        'latest',
        {
          tracer: createOpcodeCountTracer(),
          timeout: '10s',
        },
      )

      verifyJsTracerResult(res as OpcodeCountTracerResult)
    })

    it('traceTransaction with JavaScript tracer', async () => {
      const { client, sender, A } = clients

      // Send a contract transaction
      const hash = await sender.sendTransaction({
        to: A,
        data: getContractCallData(),
      })

      const receipt = await client.waitForTransactionReceipt({ hash })

      const res = await client.traceTransaction(receipt.transactionHash, {
        tracer: createOpcodeCountTracer(),
        timeout: '10s',
      })

      verifyJsTracerResult(res as OpcodeCountTracerResult)
    })
  })

  describe('simulation', () => {
    it('direct transfer to contract', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, value: 1n }],
      })
      expect(res.results[0].status).to.be.eq('success')
      expect(res.results[0].data).to.be.eq('0x')
      expectGasUSedApproximately(res.results[0].gasUsed, 25255n)
    })

    it('inner transfer', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, data: CallHelper.encodeNested({ fn: 'transfer', to: A, value: 1n }) }],
      })
      expect(res.results[0].status).to.be.eq('success')
      expect(res.results[0].data).to.be.eq('0x')
      expectGasUSedApproximately(res.results[0].gasUsed, 36563n)
    })

    it('reverted message', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, data: CallHelper.encodeNested({ fn: 'revertWithString', message: 'test' }) }],
      })
      expect(res.results[0].status).to.be.eq('failure')
      expect(res.results[0].data).to.be.eq(CallHelper.encodeRevertMessage('test'))
      expectGasUSedApproximately(res.results[0].gasUsed, 24474n)
    })

    it('inner reverted message', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [
          {
            to: A,
            data: CallHelper.encodeNested({
              fn: 'execute',
              target: A,
              data: { fn: 'revertWithString', message: 'test' },
            }),
          },
        ],
      })
      expect(res.results[0].status).to.be.eq('success')
      expect(res.results[0].data).to.be.eq(
        CallHelper.result({ success: true, nested: { success: false, revertString: 'test' } }).result,
      )
      expectGasUSedApproximately(res.results[0].gasUsed, 29623n)
    })

    it('unknown method', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, data: '0xdeadbeef' }],
      })
      expect(res.results[0].status).to.be.eq('failure')
      expect(res.results[0].data).to.be.eq('0x')
      expectGasUSedApproximately(res.results[0].gasUsed, 23350n)
    })

    it('invalid selector', async () => {
      const { sender, client, A } = clients
      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [{ to: A, data: '0xc0ffee' }],
      })
      expect(res.results[0].status).to.be.eq('failure')
      expect(res.results[0].data).to.be.eq('0x')
      expectGasUSedApproximately(res.results[0].gasUsed, 23220n)
    })

    it('static call revert', async () => {
      const { sender, client, A, B, C } = clients

      const res = await client.simulateCalls({
        account: sender.account.address,
        calls: [
          { to: A, data: CallHelper.encodeNested({ fn: 'staticCall', target: sender.account.address, value: 1n }) },
          {
            to: A,
            data: CallHelper.encodeNested({
              fn: 'staticCall',
              target: B,
              data: { fn: 'transfer', to: C, value: 1n },
              value: 1n,
            }),
            value: 1n,
          },
        ],
      })
      expect(res.results[0].status).to.be.eq('success')
      expectGasUSedApproximately(res.results[0].gasUsed, 26850n)

      expect(res.results[1].status).to.be.eq('success')
      // the gas used is not stable in arc/reth/geth
      // 10218342, 10930034, 11815139
      expect(Number(res.results[1].gasUsed)).to.be.approximately(10_930_034, 2_000_000)
    })
  })

  describe('estimation', () => {
    it('direct transfer to contract', async () => {
      const { sender, client, A } = clients
      const gasEstimated = await client.estimateGas({
        account: sender.account.address,
        to: A,
        value: 1n,
      })
      expectGasUSedApproximately(gasEstimated, 25420n)
    })

    it('direct transfer to EOA', async () => {
      const { sender, client, receiver } = clients
      const gasEstimated = await client.estimateGas({
        account: sender.account.address,
        to: receiver.account.address,
        value: 1n,
      })
      expectGasUSedApproximately(gasEstimated, 25541n, 0)
    })

    it('inner transfer', async () => {
      const { sender, client, A } = clients
      const gasEstimated = await client.estimateGas({
        account: sender.account.address,
        to: A,
        data: CallHelper.encodeNested({ fn: 'transfer', to: A, value: 1n }),
      })
      expectGasUSedApproximately(gasEstimated, 36926n)
    })

    it('reverted message', async () => {
      const { sender, client, A } = clients
      await expect(
        client.estimateGas({
          account: sender.account.address,
          to: A,
          data: CallHelper.encodeNested({ fn: 'revertWithString', message: 'test' }),
        }),
      ).to.be.rejectedWith(EstimateGasExecutionError, /reverted with reason: test/)
    })

    it('inner reverted message', async () => {
      const { sender, client, A } = clients
      const gasEstimated = await client.estimateGas({
        account: sender.account.address,
        to: A,
        data: CallHelper.encodeNested({
          fn: 'execute',
          target: A,
          data: { fn: 'revertWithString', message: 'test' },
        }),
      })
      expectGasUSedApproximately(gasEstimated, 29972n)
    })

    it('unknown method', async () => {
      const { sender, client, A } = clients
      await expect(
        client.estimateGas({ account: sender.account.address, to: A, data: '0xdeadbeef' }),
      ).to.be.rejectedWith(EstimateGasExecutionError, /reverted for an unknown reason/)
    })

    it('balance check if value exceed balance', async () => {
      const { sender, client, receiver } = clients
      const balc = await client.getBalance({ address: sender.account.address })
      await expect(
        client.estimateGas({ account: sender.account.address, to: receiver.account.address, value: balc + 3n }),
      ).to.be.rejectedWith(EstimateGasExecutionError, 'insufficient funds for gas * price + value')
    })

    it('balance check if gas price is provided', async () => {
      const { sender, client, receiver } = clients
      const balc = await client.getBalance({ address: sender.account.address })
      const block = await client.getBlock()

      // geth return "insufficient funds for transfer"
      // reth return "Missing or invalid parameters"
      await expect(
        client.estimateGas({
          account: sender.account.address,
          to: receiver.account.address,
          value: balc - 21000n,
          maxFeePerGas: block.baseFeePerGas || 1n, // latest block base fee
        }),
      ).to.be.rejectedWith(EstimateGasExecutionError, /(Missing or invalid parameters|insufficient funds for transfer)/)
    })
  })

  describe('eth_call with state overrides', () => {
    it('override account balance', async () => {
      const { client, sender, receiver } = clients
      const currentBalance = await client.getBalance({ address: sender.account.address })
      const largeValue = currentBalance + parseEther('1.0') // More than current balance
      const sufficientBalance = largeValue + parseEther('1.0') // Enough to cover the call

      // Call without override - should fail due to insufficient balance
      await expect(
        client.call({
          to: receiver.account.address,
          value: largeValue,
        }),
      ).to.be.rejectedWith(/insufficient funds/)

      // Call with overridden balance - should succeed
      const result = await client.call({
        to: receiver.account.address,
        value: largeValue,
        blockTag: 'latest',
        stateOverride: [
          {
            address: sender.account.address,
            balance: sufficientBalance,
          },
        ],
      })
      // empty result is expected for transfers
      expect(result).to.exist
    })

    it('override contract storage', async () => {
      const { client, A, randSlot } = clients
      const slot = randSlot()
      const overrideValue = 12345

      // First verify the storage slot is empty
      const originalResult = await client.call({
        to: A,
        data: CallHelper.encodeNested({ fn: 'getStorage', slot }),
      })
      expect(originalResult.data).to.be.eq(toHex(0, { size: 32 }))

      // Call with overridden storage
      const overrideResult = await client.call({
        to: A,
        data: CallHelper.encodeNested({ fn: 'getStorage', slot }),
        blockTag: 'latest',
        stateOverride: [
          {
            address: A,
            stateDiff: [
              {
                slot: toHex(slot, { size: 32 }),
                value: toHex(overrideValue, { size: 32 }),
              },
            ],
          },
        ],
      })

      // Success is indicated by getting a valid response (not throwing)
      expect(overrideResult.data).to.not.be.undefined
      expect(overrideResult.data).to.be.eq(toHex(overrideValue, { size: 32 }))
    })

    it('override contract code', async () => {
      const { client, A } = clients

      // First verify the original function works
      const originalResult = await client.call({
        to: A,
        data: CallHelper.encodeNested({ fn: 'getBlockInfo' }),
      })
      expect(originalResult.data).to.not.be.undefined

      // Simple contract that always reverts with "replaced contract"
      const replacementCode =
        '0x608060405234801561001057600080fd5b506004361061002b5760003560e01c80630dfe168014610030575b600080fd5b610038610048565b6040516100459190610067565b60405180910390f35b60606040517f08c379a000000000000000000000000000000000000000000000000000000000815260040161007e90610082565b60405180910390fd5b600060208201905081810360008301526100a1816100a7565b9050919050565b7f7265706c6163656420636f6e7472616374000000000000000000000000000000600082015250565b6000602082019050919050565b60008190505056'

      // Call with replaced code - should revert
      await expect(
        client.call({
          to: A,
          data: CallHelper.encodeNested({ fn: 'getBlockInfo' }),
          blockTag: 'latest',
          stateOverride: [
            {
              address: A,
              code: replacementCode,
            },
          ],
        }),
      ).to.be.rejectedWith(/execution reverted/)
    })
  })

  describe('BlockHashHistory (EIP-2935)', () => {
    const HISTORY_STORAGE_ADDRESS = '0x0000F90827F1C53a10cb7A02335B175320002935' as const

    it('should return non-zero hash for a recent block', async () => {
      const { client } = clients
      const blockNumber = await client.getBlockNumber()
      // Query a block that exists and is within the 8191 window
      const queryBlock = blockNumber > 1n ? blockNumber - 1n : 1n
      const calldata = toHex(queryBlock, { size: 32 })
      const result = await client.call({
        to: HISTORY_STORAGE_ADDRESS,
        data: calldata,
      })
      // The result should be a 32-byte block hash, non-zero
      expect(result.data).to.have.lengthOf(66) // '0x' + 64 hex chars
      expect(result.data).to.not.equal(toHex(0n, { size: 32 }))
    })

    it('should return the correct block hash', async () => {
      const { client } = clients
      const blockNumber = await client.getBlockNumber()
      const queryBlock = blockNumber > 1n ? blockNumber - 1n : 1n
      const expectedHash = (await client.getBlock({ blockNumber: queryBlock }))?.hash
      expect(expectedHash).to.exist

      const calldata = toHex(queryBlock, { size: 32 })
      const result = await client.call({
        to: HISTORY_STORAGE_ADDRESS,
        data: calldata,
      })
      expect(result.data).to.equal(expectedHash)
    })

    it('should revert for out-of-range block number', async () => {
      const { client } = clients
      const blockNumber = await client.getBlockNumber()
      // Query a block far in the future
      const futureBlock = blockNumber + 10000n
      const calldata = toHex(futureBlock, { size: 32 })
      await expect(client.call({ to: HISTORY_STORAGE_ADDRESS, data: calldata })).to.be.rejected
    })

    it('should have code deployed at the expected address', async () => {
      const { client } = clients
      const code = await client.getCode({ address: HISTORY_STORAGE_ADDRESS })
      expect(code).to.not.equal('0x')
      expect(code!.length).to.be.greaterThan(2)
    })
  })

  describe('selfdestruct', () => {
    it('selfdestruct locally then transfer', async () => {
      const { client, sender, A, receiver } = clients
      const artifact = await hre.artifacts.readArtifact('NativeTransferHelper')
      const amount1 = parseEther('0.01')
      const amount2 = parseEther('0.02')

      const deploySelfDestruct = encodeDeployData({
        abi: artifact.abi,
        bytecode: artifact.bytecode,
        args: [receiver.account.address, true],
      })
      const salt = BigInt(Date.now())
      const nativeHelper = getCreate2Address({
        from: A,
        salt: toHex(salt, { size: 32 }),
        bytecodeHash: keccak256(deploySelfDestruct),
      })

      const balances = await balancesSnapshot(client, {
        sender,
        receiver,
        nativeHelper,
        A,
      })

      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              {
                target: A,
                data: encodeFunctionData({
                  abi: CallHelper.abi,
                  functionName: 'create2',
                  args: [deploySelfDestruct, toHex(salt, { size: 32 })],
                }),
                value: amount1,
              },
              {
                // should revert because the nativeHelper is destroyed locally
                target: nativeHelper,
                value: amount2,
                allowFailure: true,
              },
              {
                // should revert because the nativeHelper is destroyed locally
                target: USDC.address,
                data: encodeFunctionData({
                  abi: USDC.abi,
                  functionName: 'transfer',
                  args: [nativeHelper, USDC.fromNative(amount2).roundDown],
                }),
                allowFailure: true,
              },
            ],
          }),
          value: amount1 + amount2,
        })
        .then((hash) => ReceiptVerifier.waitSuccess(hash))

      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 + amount2 })
          // EIP-7708: self-transfer (A → A) emits no Transfer log
          .expectNativeTransfer({ from: A, to: nativeHelper, amount: amount1 })
          .expectNativeTransfer({ from: nativeHelper, to: receiver, amount: amount1 })
          .expectExecutionResult({ helper: A, success: true, result: addressToBytes32(nativeHelper) })
          .expectExecutionResult({ helper: A, success: false, revertString: errSelfDestructBalance })
          .expectExecutionResult({ helper: A, success: false, revertString: errSelfDestructBalance })
          .expectAllEventsMatched()
      })

      await balances
        .decrease({ sender: amount1 + amount2 + receipt.totalFee() })
        .increase({ receiver: amount1, A: amount2 })
        .verify(receipt.transactionHash)

      // Confirm that it is possible to create to the same address again
      const receipt2 = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              {
                target: A,
                data: encodeFunctionData({
                  abi: CallHelper.abi,
                  functionName: 'create2',
                  args: [deploySelfDestruct, toHex(salt, { size: 32 })],
                }),
                value: amount1,
              },
            ],
          }),
          value: amount1,
        })
        .then((hash) => ReceiptVerifier.waitSuccess(hash))

      receipt2.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 })
          // EIP-7708: self-transfer (A → A) emits no Transfer log
          .expectNativeTransfer({ from: A, to: nativeHelper, amount: amount1 })
          .expectNativeTransfer({ from: nativeHelper, to: receiver, amount: amount1 })
          .expectExecutionResult({ helper: A, success: true, result: addressToBytes32(nativeHelper) })
          .expectAllEventsMatched()
      })
      await balances
        .decrease({ sender: amount1 + receipt2.totalFee() })
        .increase({ receiver: amount1 })
        .verify(receipt2.transactionHash)
    })

    it('selfdestruct to local destructed account', async () => {
      const { client, sender, A, receiver } = clients
      const artifact = await hre.artifacts.readArtifact('NativeTransferHelper')
      const amount1 = parseEther('0.01')
      const amount2 = parseEther('0.02')
      const amount3 = parseEther('0.03')

      const deploySelfDestruct = encodeDeployData({
        abi: artifact.abi,
        bytecode: artifact.bytecode,
        args: [receiver.account.address, true],
      })
      const salt = BigInt(Date.now())
      const nativeHelper = getCreate2Address({
        from: A,
        salt: toHex(salt, { size: 32 }),
        bytecodeHash: keccak256(deploySelfDestruct),
      })
      const deploySelfDestruct2 = encodeDeployData({
        abi: artifact.abi,
        bytecode: artifact.bytecode,
        args: [nativeHelper, true],
      })

      const balances = await balancesSnapshot(client, {
        sender,
        receiver,
        nativeHelper,
        A,
      })

      const receipt = await sender
        .sendTransaction({
          to: A,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              {
                target: A,
                data: encodeFunctionData({
                  abi: CallHelper.abi,
                  functionName: 'create2',
                  args: [deploySelfDestruct, toHex(salt, { size: 32 })],
                }),
                value: amount1,
              },
              {
                // should revert because the nativeHelper is destroyed locally
                target: A,
                data: encodeFunctionData({
                  abi: CallHelper.abi,
                  functionName: 'create2',
                  args: [deploySelfDestruct2, toHex(salt, { size: 32 })],
                }),
                value: amount1,
                allowFailure: true,
              },
              {
                target: receiver.account.address,
                value: amount3,
              },
            ],
          }),
          value: amount1 + amount2 + amount3,
          gas: 300000n,
        })
        .then((hash) => ReceiptVerifier.waitSuccess(hash))

      receipt.verifyEvents((ev) => {
        ev.expectNativeTransfer({ from: sender, to: A, amount: amount1 + amount2 + amount3 })
          // EIP-7708: self-transfer (A → A) emits no Transfer log
          .expectNativeTransfer({ from: A, to: nativeHelper, amount: amount1 })
          .expectNativeTransfer({ from: nativeHelper, to: receiver, amount: amount1 })
          .expectExecutionResult({ helper: A, success: true, result: addressToBytes32(nativeHelper) })
          // the revert reason could not be catch by create2, use debug trace to verify later.
          .expectExecutionResult({ helper: A, success: false, result: '0x' })
          .expectNativeTransfer({ from: A, to: receiver, amount: amount3 })
          .expectExecutionResult({ helper: A, success: true, result: '0x' })
          .expectAllEventsMatched()
      })

      const trace = await client.traceTransaction(receipt.transactionHash, { tracer: 'callTracer' })
      expect(trace?.calls?.[1]?.calls?.[0]?.revertReason).to.be.eq(errSelfDestructBalance)

      await balances
        .decrease({ sender: amount1 + amount2 + amount3 + receipt.totalFee() })
        .increase({ receiver: amount1 + amount3, A: amount2 })
        .verify(receipt.transactionHash)
    })
  })
})
