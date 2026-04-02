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
import { getChain } from '../../scripts/hardhat/viem-helper'
import { ProtocolConfig, loadGenesisConfig, type FeeParams, type ConsensusParams } from '../helpers'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { Address, decodeFunctionResult, encodeFunctionData, parseAbi, zeroAddress } from 'viem'
import { multicall3Address } from '../../scripts/genesis'
import { schemaHex } from '../../scripts/genesis/types'

describe('ProtocolConfig simulation', () => {
  const genesisConfig = loadGenesisConfig()
  const protocolConfigGenesis = genesisConfig?.ProtocolConfig
  if (!protocolConfigGenesis) {
    throw new Error('ProtocolConfig genesis config is required for simulation tests')
  }

  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const protocolConfig = ProtocolConfig.attach(client)
    const randomWallet = privateKeyToAccount(generatePrivateKey())
    const extraAbi = parseAbi(['function upgradeTo(address newImplementation)'])

    return { client, randomWallet, protocolConfig, extraAbi }
  }

  it('migrate contract', async () => {
    const { client, protocolConfig, extraAbi } = await clients()

    const code = await client.getCode({ address: ProtocolConfig.address })
    expect(code).to.not.be.empty

    const proxyAdmin = await protocolConfig.read.admin()
    expect(proxyAdmin).to.not.eq(zeroAddress)
    const res = await client.simulateCalls({
      account: proxyAdmin,
      calls: [
        {
          to: ProtocolConfig.address,
          data: encodeFunctionData({ abi: extraAbi, functionName: 'upgradeTo', args: [multicall3Address] }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
  })

  describe('ProtocolConfig Network Parameter Validation', () => {
    it('should verify network block miner matches ProtocolConfig beneficiary', async () => {
      const { client, randomWallet, protocolConfig } = await clients()
      const beneficiary = await protocolConfig.read.rewardBeneficiary()
      const controller = await protocolConfig.read.controller()

      // Simulate transactions without changing state
      const result = await client.simulateBlocks({
        blocks: [
          {
            calls: [
              {
                account: controller,
                to: ProtocolConfig.address,
                data: encodeFunctionData({
                  abi: protocolConfig.abi,
                  functionName: 'updateRewardBeneficiary',
                  args: [randomWallet.address], // Update beneficiary to random wallet for testing
                }),
              },
              // Immediately query the updated beneficiary to verify the change
              {
                account: controller,
                to: ProtocolConfig.address,
                data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'rewardBeneficiary', args: [] }),
              },
            ],
          },
        ],
      })

      // Verify simulated blocks have the correct beneficiary as miner and all calls succeed
      expect(result).to.have.length(1)
      const block = result[0]
      expect(block.calls.length).to.equal(2, 'Block should have 2 calls')

      // Verify all calls in this block succeeded
      block.calls.forEach((call, callIndex) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${callIndex} should succeed`)
      })

      expect(block.miner).to.not.be.undefined
      expect(block.miner).to.addressEqual(
        beneficiary,
        `Simulated block miner (${block.miner}) should use original beneficiary (${beneficiary})`,
      )

      // verify the beneficiary read call returns the updated value
      const readCall = block.calls[1] // Second call is the read
      expect(readCall.data).to.not.be.undefined
      const returnedBeneficiary = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'rewardBeneficiary',
        data: schemaHex.parse(readCall.data),
      })
      // Verify the contract state was actually updated
      expect(returnedBeneficiary).to.addressEqual(
        randomWallet.address,
        'Contract should return updated beneficiary address',
      )
    })
  })

  describe('ProtocolConfig role wallet validation', () => {
    it('pauser wallet from genesis config can pause and unpause via simulation', async function () {
      const pauserWallet = protocolConfigGenesis?.pauser
      expect(pauserWallet).to.not.be.undefined

      const { client, protocolConfig } = await clients()
      const [onchainPauser, isPaused] = await Promise.all([protocolConfig.read.pauser(), protocolConfig.read.paused()])
      expect(onchainPauser).to.addressEqual(pauserWallet, 'on-chain pauser differs from genesis config')

      const pauseCall = {
        account: pauserWallet,
        to: ProtocolConfig.address,
        data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'pause', args: [] }),
      }
      const unpauseCall = {
        account: pauserWallet,
        to: ProtocolConfig.address,
        data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'unpause', args: [] }),
      }
      const pausedReadCall = () => ({
        account: pauserWallet,
        to: ProtocolConfig.address,
        data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'paused', args: [] }),
      })

      const calls: Array<{ account: Address; to: Address; data: `0x${string}` }> = []

      if (isPaused) {
        calls.push(unpauseCall)
      }

      calls.push(pauseCall)
      const pauseReadIndex = calls.length
      calls.push(pausedReadCall())

      const result = await client.simulateBlocks({ blocks: [{ calls }] })
      const executedCalls = result[0]?.calls ?? []
      expect(executedCalls.length).to.equal(calls.length, 'all calls should execute')

      executedCalls.forEach((call, idx) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${idx} should succeed`)
      })

      const pauseRead = executedCalls[pauseReadIndex]?.data
      expect(pauseRead).to.not.be.undefined
      const parsedPaused = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'paused',
        data: schemaHex.parse(pauseRead),
      })
      expect(parsedPaused).to.equal(true, 'ProtocolConfig should report paused after pause()')
    })

    it('controller wallet from genesis config can update controller-only fields', async function () {
      const controllerWallet = protocolConfigGenesis?.controller
      expect(controllerWallet).to.not.be.undefined

      const { client, protocolConfig, randomWallet } = await clients()
      const [onchainController] = await Promise.all([
        protocolConfig.read.controller(),
        protocolConfig.read.rewardBeneficiary(),
      ])
      expect(onchainController).to.addressEqual(controllerWallet, 'on-chain controller differs from genesis config')

      const beneficiaryReadCall = {
        account: controllerWallet,
        to: ProtocolConfig.address,
        data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'rewardBeneficiary', args: [] }),
      }

      const calls = [
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({
            abi: protocolConfig.abi,
            functionName: 'updateRewardBeneficiary',
            args: [randomWallet.address],
          }),
        },
        beneficiaryReadCall,
      ]

      const result = await client.simulateBlocks({ blocks: [{ calls }] })
      const executedCalls = result[0]?.calls ?? []
      expect(executedCalls.length).to.equal(calls.length)

      executedCalls.forEach((call, idx) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${idx} should succeed`)
      })

      const benecifiaryRead = executedCalls[1]?.data
      expect(benecifiaryRead).to.not.be.undefined

      const newBeneficiary = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'rewardBeneficiary',
        data: schemaHex.parse(benecifiaryRead),
      })

      expect(newBeneficiary).to.addressEqual(randomWallet.address)
    })

    it('controller can push fee params near new upper bounds', async function () {
      const { client, protocolConfig } = await clients()
      const [originalFeeParams, chainController] = await Promise.all([
        protocolConfig.read.feeParams(),
        protocolConfig.read.controller(),
      ])

      const controllerWallet = protocolConfigGenesis.controller ?? chainController
      expect(controllerWallet).to.not.be.undefined
      expect(chainController).to.addressEqual(
        protocolConfigGenesis.controller,
        'On-chain controller does not match genesis config',
      )

      const updatedParams: FeeParams = {
        ...originalFeeParams,
        inverseElasticityMultiplier: 9999n,
        kRate: 9998n,
      }

      const calls = [
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({
            abi: protocolConfig.abi,
            functionName: 'updateFeeParams',
            args: [updatedParams],
          }),
        },
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'feeParams', args: [] }),
        },
      ]

      const result = await client.simulateBlocks({ blocks: [{ calls }] })
      const executedCalls = result[0]?.calls ?? []
      expect(executedCalls.length).to.equal(calls.length)

      executedCalls.forEach((call, idx) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${idx} should succeed`)
      })

      const updatedRead = executedCalls[1]?.data
      expect(updatedRead).to.not.be.undefined

      const parsedUpdated = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'feeParams',
        data: schemaHex.parse(updatedRead),
      })

      expect(parsedUpdated.inverseElasticityMultiplier).to.equal(updatedParams.inverseElasticityMultiplier)
      expect(parsedUpdated.kRate).to.equal(updatedParams.kRate)
    })

    it('controller can update blockGasLimit via dedicated setter', async function () {
      const controllerWallet = protocolConfigGenesis?.controller
      expect(controllerWallet).to.not.be.undefined

      const { client, protocolConfig } = await clients()
      const feeParams = (await protocolConfig.read.feeParams()) as FeeParams
      const newLimit = feeParams.blockGasLimit + 1_000_000n

      const calls = [
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({
            abi: protocolConfig.abi,
            functionName: 'updateBlockGasLimit',
            args: [newLimit],
          }),
        },
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'feeParams', args: [] }),
        },
      ]

      const result = await client.simulateBlocks({ blocks: [{ calls }] })
      const executedCalls = result[0]?.calls ?? []
      expect(executedCalls.length).to.equal(calls.length)
      executedCalls.forEach((call, idx) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${idx} should succeed`)
      })

      const feeParamsData = executedCalls[1]?.data
      expect(feeParamsData).to.not.be.undefined
      const updatedParams = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'feeParams',
        data: schemaHex.parse(feeParamsData ?? '0x'),
      }) as FeeParams
      expect(updatedParams.blockGasLimit).to.equal(newLimit)
    })

    it('controller can update timeoutProposeMs via dedicated setter', async function () {
      const controllerWallet = protocolConfigGenesis?.controller
      expect(controllerWallet).to.not.be.undefined

      const { client, protocolConfig } = await clients()
      const newTimeout = 30_000

      const calls = [
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({
            abi: protocolConfig.abi,
            functionName: 'updateTimeoutProposeMs',
            args: [newTimeout],
          }),
        },
        {
          account: controllerWallet,
          to: ProtocolConfig.address,
          data: encodeFunctionData({ abi: protocolConfig.abi, functionName: 'consensusParams', args: [] }),
        },
      ]

      const result = await client.simulateBlocks({ blocks: [{ calls }] })
      const executedCalls = result[0]?.calls ?? []
      expect(executedCalls.length).to.equal(calls.length)
      executedCalls.forEach((call, idx) => {
        expect(call.error).to.be.undefined
        expect(call.status).to.equal('success', `call ${idx} should succeed`)
      })

      const consensusData = executedCalls[1]?.data
      expect(consensusData).to.not.be.undefined
      const updatedConsensus = decodeFunctionResult({
        abi: protocolConfig.abi,
        functionName: 'consensusParams',
        data: schemaHex.parse(consensusData ?? '0x'),
      }) as ConsensusParams
      expect(updatedConsensus.timeoutProposeMs).to.equal(BigInt(newTimeout))
    })
  })
})
