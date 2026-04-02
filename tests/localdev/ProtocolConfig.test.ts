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
import { Address, fromHex, parseEther, TransactionReceipt } from 'viem'
import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts'
import { PublicClient, WalletClient } from '@nomicfoundation/hardhat-viem/types'
import {
  balancesSnapshot,
  ReceiptVerifier,
  expectAddressEq,
  ProtocolConfig,
  getClients,
  AdminUpgradeableProxy,
  GasGuzzler,
  SystemAccounting,
  DeterministicDeployerProxy,
  gasGuzzlerArtifact,
  revertingProtocolConfigArtifact,
  calc1559BaseFee,
} from '../helpers'
import { protocolConfigAddress } from '../../scripts/genesis'

describe('ProtocolConfig Smoke Tests', function () {
  let publicClient: PublicClient
  let controller: WalletClient
  let sender: WalletClient
  let receiver: WalletClient
  let proxyAdmin: WalletClient

  // Deployed mocks
  let mockRevertingProtocolConfigAddress: Address
  let gasGuzzlerAddress: Address

  // Test accounts for different scenarios
  const testAccounts = {
    beneficiary1: privateKeyToAccount(generatePrivateKey()),
    beneficiary2: privateKeyToAccount(generatePrivateKey()),
    beneficiary3: privateKeyToAccount(generatePrivateKey()),
  }

  // Helper to restore original state
  async function restoreOriginalBeneficiary(originalBeneficiary: Address) {
    const protocolConfig = ProtocolConfig.attach(publicClient)
    const currentBeneficiary = await protocolConfig.read.rewardBeneficiary()
    if (currentBeneficiary.toLowerCase() !== originalBeneficiary.toLowerCase()) {
      await ProtocolConfig.attach(controller)
        .write.updateRewardBeneficiary([originalBeneficiary])
        .then((hash) => publicClient.waitForTransactionReceipt({ hash }))
    }
  }

  // Helper to update beneficiary and verify
  async function updateAndVerifyBeneficiary(newBeneficiary: Address): Promise<Address> {
    const protocolConfig = ProtocolConfig.attach(publicClient)
    const originalBeneficiary = await protocolConfig.read.rewardBeneficiary()

    await ProtocolConfig.attach(controller)
      .write.updateRewardBeneficiary([newBeneficiary])
      .then((hash) => publicClient.waitForTransactionReceipt({ hash }))

    const updatedBeneficiary = await protocolConfig.read.rewardBeneficiary()
    expectAddressEq(updatedBeneficiary, newBeneficiary, 'Beneficiary should be updated')

    return originalBeneficiary
  }

  // Helper to update the block gas limit
  async function updateBlockGasLimit(newLimit: bigint): Promise<TransactionReceipt> {
    const protocolConfigReader = ProtocolConfig.attach(publicClient)
    const params = await protocolConfigReader.read.feeParams()
    const updated = { ...params, blockGasLimit: newLimit }
    const hash = await ProtocolConfig.attach(controller).write.updateFeeParams([updated])
    return await publicClient.waitForTransactionReceipt({ hash })
  }

  // Helper to update the base fee bounds
  async function updateBaseFeeBounds(newMin: bigint, newMax: bigint): Promise<TransactionReceipt> {
    const protocolConfigReader = ProtocolConfig.attach(publicClient)
    const params = await protocolConfigReader.read.feeParams()
    const updated = { ...params, minBaseFee: newMin, maxBaseFee: newMax }
    const hash = await ProtocolConfig.attach(controller).write.updateFeeParams([updated])
    const receipt = await publicClient.waitForTransactionReceipt({ hash })
    ReceiptVerifier.build(receipt).isSuccess()
    return receipt
  }

  // Helper to send transaction and get block
  async function sendTransactionAndGetBlock(value: bigint, maxFeePerGas: bigint, maxPriorityFeePerGas: bigint) {
    const txHash = await sender.sendTransaction({
      to: receiver.account.address,
      value,
      maxFeePerGas,
      maxPriorityFeePerGas,
    })

    const receipt = await publicClient.waitForTransactionReceipt({ hash: txHash })
    const block = await publicClient.getBlock({ blockNumber: receipt.blockNumber })

    return { txHash, receipt, block }
  }

  // Helper to send transaction, verify miner, and check balance changes
  async function sendTransactionAndVerifyBalances(params: {
    beneficiary: Address
    transferAmount?: bigint
    gasPrice?: bigint
    maxFeePerGas?: bigint
    maxPriorityFeePerGas?: bigint
    transactionType?: 'legacy' | 'eip1559'
  }) {
    const {
      beneficiary,
      transferAmount = parseEther('0.01'), // Default transfer amount
      maxFeePerGas,
      maxPriorityFeePerGas,
      transactionType = 'legacy',
    } = params

    // Let Viem estimate optimal gas parameters - this adapts to current network conditions
    // and provides more reliable gas estimation than hardcoded values
    const baseTransaction = {
      to: receiver.account.address,
      value: transferAmount,
      account: sender.account,
    } as const

    // Estimate gas limit for the transaction
    const estimatedGas = await publicClient.estimateGas(baseTransaction)

    // Get suggested gas pricing based on current network conditions
    let gasSettings: Record<string, unknown>
    if (transactionType === 'eip1559') {
      // Use EIP-1559 fee estimation with smart base fee and priority fee calculation
      const feeData = await publicClient.estimateFeesPerGas()
      gasSettings = {
        gas: estimatedGas,
        maxFeePerGas: maxFeePerGas || feeData.maxFeePerGas,
        maxPriorityFeePerGas: maxPriorityFeePerGas || feeData.maxPriorityFeePerGas,
        type: 'eip1559' as const,
      }
    } else {
      // Use Viem's legacy gas price estimation
      const suggestedGasPrice = params.gasPrice || (await publicClient.getGasPrice())
      gasSettings = {
        gas: estimatedGas,
        gasPrice: suggestedGasPrice,
      }
    }

    // Setup balance tracking
    const balances = await balancesSnapshot(publicClient, {
      beneficiary,
      sender: sender.account.address,
      receiver: receiver.account.address,
    })

    // Send transaction with Viem's optimized gas settings
    const tx = await sender.sendTransaction({
      to: receiver.account.address,
      value: transferAmount,
      ...gasSettings,
    })

    const receipt = await publicClient.waitForTransactionReceipt({ hash: tx })
    const block = await publicClient.getBlock({ blockHash: receipt.blockHash })
    const receiptVerifier = ReceiptVerifier.build(receipt)
    const totalFee = receiptVerifier.totalFee()

    // Verify block miner matches beneficiary
    if (!block.miner) {
      throw new Error('Block miner is undefined')
    }
    expectAddressEq(block.miner, beneficiary, 'Block miner should match beneficiary')

    // Verify balance changes
    await balances
      .increase({
        beneficiary: totalFee,
        receiver: transferAmount,
      })
      .decrease({
        sender: transferAmount + totalFee,
      })
      .verify()

    return { receipt, block, totalFee, receiptVerifier }
  }

  // Helper to mine a tx and return the block
  const mineBlock = async () => {
    const { block } = await sendTransactionAndGetBlock(
      parseEther('0.01'),
      1000000000000n, // 1 gwei
      100000000n, // 0.1 gwei
    )
    return block
  }

  before(async function () {
    // Setup clients
    const { client, sender: _sender, receiver: _receiver, admin, proxyAdmin: _proxyAdmin } = await getClients()
    publicClient = client
    controller = admin
    sender = _sender
    receiver = _receiver
    proxyAdmin = _proxyAdmin

    // Deploy mocks
    mockRevertingProtocolConfigAddress = await DeterministicDeployerProxy.deployCode(
      sender,
      client,
      revertingProtocolConfigArtifact.bytecode,
    )
    gasGuzzlerAddress = await DeterministicDeployerProxy.deployCode(sender, client, gasGuzzlerArtifact.bytecode)
  })

  describe('Core Integration', function () {
    it('should use ProtocolConfig beneficiary as block miner', async function () {
      // Get current beneficiary from contract
      const protocolConfig = ProtocolConfig.attach(publicClient)
      const contractBeneficiary = await protocolConfig.read.rewardBeneficiary()

      // Send transaction and verify miner and balances match contract beneficiary
      await sendTransactionAndVerifyBalances({
        beneficiary: contractBeneficiary,
      })
    })

    it('should reflect beneficiary changes in block mining', async function () {
      const originalBeneficiary = await updateAndVerifyBeneficiary(testAccounts.beneficiary1.address)

      // Verify new beneficiary is used for mining and balances
      await sendTransactionAndVerifyBalances({
        beneficiary: testAccounts.beneficiary1.address,
      })

      // Restore original state
      await restoreOriginalBeneficiary(originalBeneficiary)
    })

    it('should handle multiple beneficiary updates correctly', async function () {
      const protocolConfig = ProtocolConfig.attach(publicClient)
      const originalBeneficiary = await protocolConfig.read.rewardBeneficiary()

      // First update
      await updateAndVerifyBeneficiary(testAccounts.beneficiary1.address)
      await sendTransactionAndVerifyBalances({
        beneficiary: testAccounts.beneficiary1.address,
      })

      // Second update
      await updateAndVerifyBeneficiary(testAccounts.beneficiary2.address)
      await sendTransactionAndVerifyBalances({
        beneficiary: testAccounts.beneficiary2.address,
      })

      // Third update
      await updateAndVerifyBeneficiary(testAccounts.beneficiary3.address)
      await sendTransactionAndVerifyBalances({
        beneficiary: testAccounts.beneficiary3.address,
      })

      // Restore original state
      await restoreOriginalBeneficiary(originalBeneficiary)
    })
  })

  describe('Fee Distribution', function () {
    // Save/restore bounds per test to avoid state leak
    let originalMinBaseFee: bigint
    let originalMaxBaseFee: bigint
    let originalGasLimit: bigint

    this.beforeEach(async function () {
      const protocolConfigReader = ProtocolConfig.attach(publicClient)
      const feeParams = await protocolConfigReader.read.feeParams()
      originalMinBaseFee = feeParams.minBaseFee
      originalMaxBaseFee = feeParams.maxBaseFee
      originalGasLimit = feeParams.blockGasLimit
    })

    this.afterEach(async function () {
      await updateBaseFeeBounds(originalMinBaseFee, originalMaxBaseFee)
      await updateBlockGasLimit(originalGasLimit)
    })

    it('should handle legacy transactions correctly', async function () {
      const protocolConfig = ProtocolConfig.attach(publicClient)
      const originalBeneficiary = await protocolConfig.read.rewardBeneficiary()
      const beneficiary = testAccounts.beneficiary1.address
      await updateAndVerifyBeneficiary(beneficiary)

      // Send transaction and verify both miner and balance changes
      await sendTransactionAndVerifyBalances({
        beneficiary,
        transferAmount: parseEther('0.1'),
        transactionType: 'legacy',
      })

      // Restore original state
      await restoreOriginalBeneficiary(originalBeneficiary)
    })

    it('should handle EIP-1559 transactions correctly', async function () {
      const protocolConfig = ProtocolConfig.attach(publicClient)
      const originalBeneficiary = await protocolConfig.read.rewardBeneficiary()
      const beneficiary = testAccounts.beneficiary2.address
      await updateAndVerifyBeneficiary(beneficiary)

      // Send EIP-1559 transaction and verify both miner and balance changes
      const transferAmount = parseEther('0.05')

      const { receipt, totalFee } = await sendTransactionAndVerifyBalances({
        beneficiary,
        transferAmount,
        transactionType: 'eip1559',
      })

      // Verify Circle's custom fee distribution vs standard Ethereum
      const gasUsed = BigInt(receipt.gasUsed)
      const effectiveGasPrice = BigInt(receipt.effectiveGasPrice || 0)
      // Access baseFeePerGas which exists on EIP-1559 receipts but isn't in standard type
      const receiptWithBaseFee = receipt as { baseFeePerGas?: bigint }
      const actualBaseFee = receiptWithBaseFee.baseFeePerGas || 0n

      // Arc: Beneficiary gets FULL effective gas price * gas used
      const arcExpectedFee = effectiveGasPrice * gasUsed
      expect(totalFee).to.equal(
        arcExpectedFee,
        'Arc: Beneficiary should receive full effective gas price * gas used',
      )

      // Standard Ethereum: Beneficiary would only get (effective_gas_price - base_fee) * gas used
      const standardEthereumFee = (effectiveGasPrice - actualBaseFee) * gasUsed

      // Only assert the difference if there's actually a base fee
      if (actualBaseFee > 0n) {
        expect(totalFee).to.not.equal(
          standardEthereumFee,
          'Circle should give MORE than standard Ethereum (which burns base fee)',
        )
        expect(totalFee > standardEthereumFee).to.be.true
      }

      // Restore original state
      await restoreOriginalBeneficiary(originalBeneficiary)
    })

    it('should use EMA base fee calculation in blocks', async function () {
      const protocolConfig = ProtocolConfig.attach(publicClient)

      // Get current fee parameters to ensure EMA mode is active (alpha < 100)
      const feeParams = await protocolConfig.read.feeParams()
      expect(Number(feeParams.alpha)).to.be.lessThan(100, 'Should be in EMA mode with alpha < 100')
      const gasLimit = feeParams.blockGasLimit

      const baseFeeMax = 200000000000n
      const baseFeeMin = 1000000000n

      // Drive up the base fee to notice base fee shifts more easily
      await updateBaseFeeBounds(baseFeeMin, baseFeeMax)
      await mineBlock() // Advance again

      // Sanity check -- we're at the minimum base fee to start
      const block = await publicClient.getBlock()
      expect(block.baseFeePerGas).to.equal(baseFeeMin, `unexpect base fee for block ${block.number}`)

      // Send a large transaction consuming a significant portion of the block gas limit.
      // EIP-7825 (Osaka) caps individual tx gas at 16,777,216 (2^24), so we use the
      // minimum of 90% of the block gas limit and the EIP-7825 cap.
      const EIP7825_TX_GAS_LIMIT_CAP = 16_777_216n
      const txGas = (gasLimit * 9n) / 10n < EIP7825_TX_GAS_LIMIT_CAP ? (gasLimit * 9n) / 10n : EIP7825_TX_GAS_LIMIT_CAP

      // Without historical gas smoothing, we should expect the base fee to raise
      // With historical gas smoothing, the smoothed gas used should still be less than 1/2 the
      // block gas limit, so it should remain at the base fee mininum
      const receipt = await GasGuzzler.attach(sender, gasGuzzlerAddress)
        .write.guzzle([200n], {
          gas: txGas,
          gasPrice: await publicClient.getGasPrice(),
          value: 0n,
        })
        .then(ReceiptVerifier.waitSuccess)
        .then((v) => v.receipt)
      const txBlock = await publicClient.getBlock({ blockNumber: receipt.blockNumber })

      await mineBlock() // Advance again

      // Get the block immediately following the large tx
      const nextBlock = await publicClient.getBlock({ blockNumber: txBlock.number + 1n })
      const actualNextBaseFee = Number(nextBlock.baseFeePerGas || 0n)

      // Calculate what standard EIP-1559 would produce
      const standardNextBaseFee = calc1559BaseFee(txBlock.gasUsed, txBlock.gasLimit, txBlock.baseFeePerGas || 0n)

      // Retrieve the smoothed parent gas used
      const { gasUsed, gasUsedSmoothed, nextBaseFee } = await SystemAccounting.getGasValues(
        publicClient,
        txBlock.number,
      )
      // verify the fee in zero4
      expect(txBlock.extraData).has.length(18)
      expect(nextBaseFee).to.equal(nextBlock.baseFeePerGas)
      expect(nextBaseFee).to.equal(fromHex(txBlock.extraData, 'bigint'))

      // Sanity check that the system accounting value matches the block header
      expect(gasUsed).to.equal(txBlock.gasUsed, 'SystemAccounting gasUsed should match block header')

      // Calculate the EMA-smoothed base fee
      const emaSmoothedBaseFee = calc1559BaseFee(gasUsedSmoothed, txBlock.gasLimit, txBlock.baseFeePerGas || 0n)

      // Our expectation is that the EMA smoothed base fee value is lower
      // than the base fee floor, since the historical gas use should drag it below
      // the 1/2 gas target.
      expect(emaSmoothedBaseFee).to.be.lessThan(baseFeeMin)

      // Similarly, we expected the raw EIP-1559 gas fee to be higher
      expect(standardNextBaseFee).to.be.greaterThan(baseFeeMin)

      // Since we have base fee bounds, confirm that the actual computed base fee
      // matches the EMA-smoothed value, bounded by our limits
      expect(actualNextBaseFee).to.equal(baseFeeMin)
    })
  })

  describe('Block gas limits', function () {
    // Constants - defined in protocol_config.rs
    const MIN_BLOCK_GAS_LIMIT = 1_000_000n
    const MAX_BLOCK_GAS_LIMIT = 1_000_000_000n
    const DEFAULT_BLOCK_GAS_LIMIT = 30_000_000n

    // Used for state restoration
    let originalBlockGasLimit: bigint
    let originalTimeoutProposeMs: number

    this.beforeEach(async function () {
      // Capture original block gas limit
      const protocolConfigReader = ProtocolConfig.attach(publicClient)
      const params = await protocolConfigReader.read.feeParams()
      originalBlockGasLimit = params.blockGasLimit

      // Capture original timeoutProposeMs
      const consensusParams = await protocolConfigReader.read.consensusParams()
      originalTimeoutProposeMs = consensusParams.timeoutProposeMs
    })

    this.afterEach(async function () {
      // Clean up
      await updateBlockGasLimit(originalBlockGasLimit)
      await ProtocolConfig.attach(controller).write.updateTimeoutProposeMs([originalTimeoutProposeMs])
    })

    it('should update timeoutProposeMs via dedicated setter and still mine blocks', async function () {
      const newTimeout = 30_000
      const previousBlock = await publicClient.getBlock()

      const txHash = await ProtocolConfig.attach(controller).write.updateTimeoutProposeMs([newTimeout])
      await publicClient.waitForTransactionReceipt({ hash: txHash })

      const after = await ProtocolConfig.attach(publicClient).read.consensusParams()
      expect(after.timeoutProposeMs).to.equal(newTimeout)

      // Mine a block to ensure consensus continues after the change
      const block = await mineBlock()
      expect(block.number).to.be.greaterThan(previousBlock.number)
    })

    it('should update block gas limit via dedicated setter and reflect in params', async function () {
      const startBlock = await mineBlock()
      const startLimit = startBlock.gasLimit
      // Update block gas limit to a higher value
      const higherLimit = startLimit + 5_000_000n
      expect(higherLimit).to.be.lessThanOrEqual(MAX_BLOCK_GAS_LIMIT)

      const txHash = await ProtocolConfig.attach(controller).write.updateBlockGasLimit([higherLimit])
      await publicClient.waitForTransactionReceipt({ hash: txHash })

      // Verify header reflects updated gas limit
      const block = await mineBlock()
      expect(block.gasLimit).to.equal(higherLimit)
    })

    it('should update the block gas limit and reflect the update in block headers', async function () {
      const startBlock = await mineBlock()
      const startLimit = startBlock.gasLimit

      // Update block gas limit to a higher value
      const higherLimit = startLimit + 5_000_000n
      // Sanity check
      expect(higherLimit).to.be.lessThanOrEqual(MAX_BLOCK_GAS_LIMIT)
      await updateBlockGasLimit(higherLimit)

      // Verify header reflects updated gas limit
      let block = await mineBlock()
      expect(block.gasLimit).to.equal(higherLimit)

      // Update to a lower value
      const lowerLimit = startLimit - 5_000_000n
      // Sanity check
      expect(lowerLimit).to.be.greaterThanOrEqual(MIN_BLOCK_GAS_LIMIT)
      await updateBlockGasLimit(lowerLimit)

      // Verify header reflects updated gas limit
      block = await mineBlock()
      expect(block.gasLimit).to.equal(lowerLimit)
    })

    it('should reject transactions that exceed the block gas limit', async function () {
      // Lower the block gas limit so we can test rejection without hitting the
      // EIP-7825 per-transaction cap (16,777,216 gas) introduced by Osaka.
      const lowLimit = 5_000_000n
      await updateBlockGasLimit(lowLimit)
      await mineBlock()

      // Try to submit a transaction that exceeds the block gas limit
      const tooHighGas = lowLimit + 100_000n
      let rejected = false
      try {
        await sender.sendTransaction({
          to: receiver.account.address,
          value: 1n,
          gas: tooHighGas,
          gasPrice: await publicClient.getGasPrice(),
        })
        // If it returned a hash, wait briefly to see if the node rejected
      } catch {
        rejected = true
      }
      expect(rejected, 'tx exceeding block gas limit should be rejected').to.be.true

      // Raise the block gas limit to accept the same tx
      expect(tooHighGas).to.be.lessThanOrEqual(MAX_BLOCK_GAS_LIMIT)
      await updateBlockGasLimit(tooHighGas)
      await mineBlock()

      // Verify the same tx (with gas <= new limit) is now accepted
      const txHashOk = await sender.sendTransaction({
        to: receiver.account.address,
        value: 1n,
        gas: tooHighGas,
        gasPrice: await publicClient.getGasPrice(),
      })
      const okReceipt = await publicClient.waitForTransactionReceipt({ hash: txHashOk })
      expect(okReceipt.status).to.equal('success')
    })

    it('should fallback to default block gas limit if bounds are exceeded', async function () {
      // Verify current block gas limit
      const startBlock = await mineBlock()
      const startLimit = startBlock.gasLimit
      expect(startLimit).to.be.lessThan(MAX_BLOCK_GAS_LIMIT)

      // Update block gas limit exceeding the hard cap
      // This should be clamped to the max
      const higherLimit = MAX_BLOCK_GAS_LIMIT + 100_000n
      await updateBlockGasLimit(higherLimit)

      let block = await mineBlock()
      // Revert to default
      expect(block.gasLimit).to.equal(DEFAULT_BLOCK_GAS_LIMIT)

      // Update block gas limit to a very low value
      const lowerLimit = MIN_BLOCK_GAS_LIMIT - 100_000n
      await updateBlockGasLimit(lowerLimit)

      // Verify header reflects minimum clamp
      block = await mineBlock()
      expect(block.gasLimit).to.equal(DEFAULT_BLOCK_GAS_LIMIT)
    })

    // Skip: This test upgrades ProtocolConfig to a reverting implementation which crashes the real Malachite CL
    // TODO: Re-enable when running with mock CL only (smoke-reth) or find alternative testing approach
    it.skip('should fallback to attributes gas limit if it cannot retrieve the current value from ProtocolConfig', async function () {
      this.timeout(60000) // Increase timeout - test does multiple contract upgrades and reads
      const protocolConfigProxy = AdminUpgradeableProxy.attach(proxyAdmin, protocolConfigAddress)
      // Used to restore its state later
      const originalImplAddr = await protocolConfigProxy.read.implementation()

      // Use the ProxyAdmin role to point to a bricked implementation
      // This would cause retrieving the fee configuration to fail
      let upgradeTx = await protocolConfigProxy.write.upgradeTo([mockRevertingProtocolConfigAddress])
      let receipt = await publicClient.waitForTransactionReceipt({ hash: upgradeTx })

      const parentBlock = await publicClient.getBlock({ blockNumber: receipt.blockNumber })
      const parentBlockGasLimit = parentBlock.gasLimit

      // Loop over the next 3 blocks
      // Ensure the gas limit is frozen at the parent, even though the head advances
      let lastBlockNum = parentBlock.number
      for (let i = 0; i < 3; i++) {
        const nextBlock = await mineBlock()
        expect(nextBlock.number).to.be.greaterThan(lastBlockNum)
        expect(nextBlock.gasLimit).to.not.equal(parentBlockGasLimit)
        lastBlockNum = nextBlock.number
      }

      // Revert back to the functioning version
      upgradeTx = await protocolConfigProxy.write.upgradeTo([originalImplAddr])
      receipt = await publicClient.waitForTransactionReceipt({ hash: upgradeTx })
      await mineBlock() // Advance to the next block to trigger calculating the gas limit again
      expect((await publicClient.getBlock({ blockNumber: receipt.blockNumber + 1n })).gasLimit).to.equal(
        parentBlockGasLimit,
      )

      // Update gas limit, and ensure it takes effect
      const newGasLimit = parentBlockGasLimit + 1n
      // Sanity check, otherwise there will be no affect
      expect(parentBlockGasLimit).to.not.equal(MAX_BLOCK_GAS_LIMIT)

      // Update limit
      const updateTx = await updateBlockGasLimit(newGasLimit)

      // Should take effect in the next block(s)
      await mineBlock()
      await mineBlock()

      // Check it was updated in the subsequent block
      expect((await publicClient.getBlock({ blockNumber: updateTx.blockNumber })).gasLimit).to.equal(
        parentBlockGasLimit,
      )
      expect((await publicClient.getBlock({ blockNumber: updateTx.blockNumber + 1n })).gasLimit).to.equal(newGasLimit)
      expect((await publicClient.getBlock({ blockNumber: updateTx.blockNumber + 2n })).gasLimit).to.equal(newGasLimit)
    })
  })

  describe('Base fee bounds', function () {
    // Save/restore bounds per test to avoid state leak
    let originalMinBaseFee: bigint
    let originalMaxBaseFee: bigint
    let originalGasLimit: bigint

    this.beforeEach(async function () {
      const protocolConfigReader = ProtocolConfig.attach(publicClient)
      const feeParams = await protocolConfigReader.read.feeParams()
      originalMinBaseFee = feeParams.minBaseFee
      originalMaxBaseFee = feeParams.maxBaseFee
      originalGasLimit = feeParams.blockGasLimit
    })

    this.afterEach(async function () {
      await updateBaseFeeBounds(originalMinBaseFee, originalMaxBaseFee)
      await updateBlockGasLimit(originalGasLimit)
    })

    async function checkBaseFee(blockHeight: bigint, expectedBaseFee: bigint) {
      const block = await publicClient.getBlock({ blockNumber: blockHeight })
      expect(block.baseFeePerGas || 0n).to.equal(expectedBaseFee)
    }

    it('clamps down to the configured max in the next block', async function () {
      const head = await publicClient.getBlock()
      const currentBase = head.baseFeePerGas || 0n
      expect(currentBase).to.be.greaterThan(0n)

      // Choose bounds strictly below current base
      const newMax = currentBase / 2n
      const newMin = newMax / 2n
      expect(newMax).to.be.greaterThan(newMin)
      expect(newMin).to.be.greaterThan(0n)

      const updateTx = await updateBaseFeeBounds(newMin, newMax)
      // Base fee at time of update
      const baseFeeAtUpdate = (await publicClient.getBlock({ blockNumber: updateTx.blockNumber })).baseFeePerGas

      // Advance
      await mineBlock()

      // Should be bounded afterwards
      expect(baseFeeAtUpdate).to.greaterThan(newMax)
      await checkBaseFee(updateTx.blockNumber + 1n, newMax)
    })

    it('clamps up to the configured min in the next block', async function () {
      const head = await publicClient.getBlock()
      const currentBase = head.baseFeePerGas || 0n
      expect(currentBase).to.be.greaterThan(0n)

      // Choose bounds strictly above current base; ensure non-negative
      const newMin = currentBase * 2n
      const newMax = newMin * 2n
      expect(newMax).to.be.greaterThan(newMin)

      const updateTx = await updateBaseFeeBounds(newMin, newMax)
      const baseFeeAtUpdate = (await publicClient.getBlock({ blockNumber: updateTx.blockNumber })).baseFeePerGas

      // Advance
      await mineBlock()

      // Should be bounded afterwards
      expect(baseFeeAtUpdate).to.be.lessThan(newMin)
      await checkBaseFee(updateTx.blockNumber + 1n, newMin)
    })

    it('does not exceed the configured max across multiple blocks', async function () {
      // Set block gas limit low, to trigger base fee increases from smaller transactions
      const gasLimit = 1_000_000n
      await updateBlockGasLimit(gasLimit)

      const head = await publicClient.getBlock()
      const currentBaseFee = head.baseFeePerGas || 0n

      // Set a tight cap near current to observe saturation
      const maxBaseFee = currentBaseFee + 5n
      const minBaseFee = currentBaseFee
      await updateBaseFeeBounds(minBaseFee, maxBaseFee)

      // Mine several blocks, trying to push usage
      const guzzler = GasGuzzler.attach(sender, gasGuzzlerAddress)
      for (let i = 0; i < 5; i++) {
        const receipt = await guzzler.write
          .guzzle([200n], {
            gas: (gasLimit * 9n) / 10n,
            gasPrice: await publicClient.getGasPrice(),
            value: 0n,
          })
          .then(ReceiptVerifier.waitSuccess)
          .then((v) => v.receipt)

        const block = await publicClient.getBlock({ blockNumber: receipt.blockNumber })
        const baseFee = block.baseFeePerGas || 0n

        expect(baseFee).to.be.lessThanOrEqual(maxBaseFee)
        expect(baseFee).to.be.greaterThanOrEqual(minBaseFee)
      }
    })

    it('base fee updates are reflected in fee history', async function () {
      const head = await publicClient.getBlock()
      const originalBaseFee = head.baseFeePerGas || 0n

      // Set a tight cap near current to observe saturation
      const maxBaseFee = originalBaseFee + 10n
      const minBaseFee = originalBaseFee + 10n
      const updateReceipt = await updateBaseFeeBounds(minBaseFee, maxBaseFee)
      const baseFeeAtUpdateBlock =
        (await publicClient.getBlock({ blockNumber: updateReceipt.blockNumber })).baseFeePerGas || 0n

      await mineBlock()
      await mineBlock()
      await mineBlock()

      const currentBlock = await publicClient.getBlock()
      const feeHistory = await publicClient.getFeeHistory({
        blockNumber: currentBlock.number, // Note: feeHistory "walks back" from this value
        blockCount: Number(currentBlock.number - updateReceipt.blockNumber + 1n),
        rewardPercentiles: [],
      })

      // First retrieved base fee matches the block at which the params were updated
      expect(baseFeeAtUpdateBlock).to.equal(feeHistory.baseFeePerGas[0])
      // Next blocks include the adjusted fee; the last item is ignored as it is a 1559-calculated prediction
      for (let i = 1; i < feeHistory.baseFeePerGas.length - 1; i++) {
        expect(feeHistory.baseFeePerGas[i]).to.equal(maxBaseFee)
      }

      // verify zero4 fee
      expect(currentBlock.extraData).to.has.length(18)
      expect(feeHistory.baseFeePerGas[feeHistory.baseFeePerGas.length - 1]).to.equal(
        fromHex(currentBlock.extraData, 'bigint'),
      )
    })
  })
})
