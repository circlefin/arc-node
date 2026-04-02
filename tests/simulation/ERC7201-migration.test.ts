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
import { ProtocolConfig } from '../helpers'
import { loadGenesisConfig } from '../helpers'
import { parseAbi, Address, decodeFunctionResult, encodeFunctionData, Hex } from 'viem'
import fs from 'fs'

describe('ProtocolConfig ERC-7201 Migration', () => {
  const genesisConfig = loadGenesisConfig()
  if (!genesisConfig) {
    return
  }

  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const protocolConfig = ProtocolConfig.attach(client)
    return { client, protocolConfig }
  }

  const getNewImplementation = () => process.env.NEW_IMPLEMENTATION_ADDRESS as Address | undefined
  const newImplementation = getNewImplementation() as Address

  ;(newImplementation && genesisConfig?.ProtocolConfig?.proxy?.admin ? describe : describe.skip)(
    'Migration Tests',
    () => {
      it('should verify new implementation contract is deployed', async () => {
        const { client } = await clients()

        const deployedCode = await client.getCode({ address: newImplementation })
        expect(deployedCode, `No code found at ${newImplementation}`).to.not.be.undefined
        expect(deployedCode).to.not.equal('0x')

        const forgeArtifactPath = 'contracts/out/forge/ProtocolConfig.sol/ProtocolConfig.json'
        const forgeArtifact = JSON.parse(fs.readFileSync(forgeArtifactPath, 'utf8')) as {
          deployedBytecode: { object: Hex }
        }
        const forgeBytecode = forgeArtifact.deployedBytecode.object

        expect(deployedCode).to.equal(forgeBytecode)
      })

      it('should verify new implementation has expected interface', async () => {
        const { client } = await clients()

        const viewCalls = await client.multicall({
          contracts: [
            {
              address: newImplementation,
              abi: parseAbi(['function controller() view returns (address)']),
              functionName: 'controller',
            },
            {
              address: newImplementation,
              abi: parseAbi(['function pauser() view returns (address)']),
              functionName: 'pauser',
            },
            {
              address: newImplementation,
              abi: parseAbi(['function paused() view returns (bool)']),
              functionName: 'paused',
            },
            {
              address: newImplementation,
              abi: parseAbi(['function owner() view returns (address)']),
              functionName: 'owner',
            },
            {
              address: newImplementation,
              abi: parseAbi(['function rewardBeneficiary() view returns (address)']),
              functionName: 'rewardBeneficiary',
            },
          ],
          allowFailure: true,
        })

        expect(viewCalls[0].status).to.equal('success')
        expect(viewCalls[1].status).to.equal('success')
        expect(viewCalls[2].status).to.equal('success')
        expect(viewCalls[3].status).to.equal('success')
        expect(viewCalls[4].status).to.equal('success')
      })

      it('should preserve all storage values after migration', async () => {
        const { client, protocolConfig } = await clients()
        const admin = await protocolConfig.read.admin()

        const oldController = await protocolConfig.read.controller()
        const oldPauser = await protocolConfig.read.pauser()
        const oldPaused = await protocolConfig.read.paused()
        const oldOwner = await protocolConfig.read.owner()
        const oldFeeParams = await protocolConfig.read.feeParams()
        const oldBeneficiary = await protocolConfig.read.rewardBeneficiary()

        const initData = encodeFunctionData({
          abi: parseAbi(['function initialize(address,address,bool)']),
          functionName: 'initialize',
          args: [oldController, oldPauser, oldPaused],
        })

        const result = await client.simulateBlocks({
          blocks: [
            {
              calls: [
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: parseAbi(['function upgradeToAndCall(address,bytes)']),
                  functionName: 'upgradeToAndCall',
                  args: [newImplementation, initData],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'controller',
                  args: [],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'pauser',
                  args: [],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'paused',
                  args: [],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'owner',
                  args: [],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'feeParams',
                  args: [],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'rewardBeneficiary',
                  args: [],
                },
              ],
            },
          ],
        })

        const calls = result[0].calls
        expect(calls[0].status).to.equal('success')

        expect(
          decodeFunctionResult({ abi: protocolConfig.abi, functionName: 'controller', data: calls[1].data }),
        ).to.equal(oldController)
        expect(decodeFunctionResult({ abi: protocolConfig.abi, functionName: 'pauser', data: calls[2].data })).to.equal(
          oldPauser,
        )
        expect(decodeFunctionResult({ abi: protocolConfig.abi, functionName: 'paused', data: calls[3].data })).to.equal(
          oldPaused,
        )
        expect(decodeFunctionResult({ abi: protocolConfig.abi, functionName: 'owner', data: calls[4].data })).to.equal(
          oldOwner,
        )

        const newFeeParams = decodeFunctionResult({
          abi: protocolConfig.abi,
          functionName: 'feeParams',
          data: calls[5].data,
        })
        expect(newFeeParams.blockGasLimit).to.equal(oldFeeParams.blockGasLimit)

        expect(
          decodeFunctionResult({ abi: protocolConfig.abi, functionName: 'rewardBeneficiary', data: calls[6].data }),
        ).to.equal(oldBeneficiary)
      })

      it('should prevent re-initialization', async () => {
        const { client, protocolConfig } = await clients()
        const admin = await protocolConfig.read.admin()
        const controller = await protocolConfig.read.controller()
        const pauser = await protocolConfig.read.pauser()
        const paused = await protocolConfig.read.paused()

        const initData = encodeFunctionData({
          abi: parseAbi(['function initialize(address,address,bool)']),
          functionName: 'initialize',
          args: [controller, pauser, paused],
        })

        const result = await client.simulateBlocks({
          blocks: [
            {
              calls: [
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: parseAbi(['function upgradeToAndCall(address,bytes)']),
                  functionName: 'upgradeToAndCall',
                  args: [newImplementation, initData],
                },
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: parseAbi(['function initialize(address,address,bool)']),
                  functionName: 'initialize',
                  args: [controller, pauser, paused],
                },
              ],
            },
          ],
        })

        expect(result[0].calls[0].status).to.equal('success')
        expect(result[0].calls[1].status).to.equal('failure')
      })

      it('should preserve controller functionality after migration', async () => {
        const { client, protocolConfig } = await clients()
        const admin = await protocolConfig.read.admin()
        const controller = genesisConfig.ProtocolConfig.controller

        const oldController = await protocolConfig.read.controller()
        const oldPauser = await protocolConfig.read.pauser()
        const oldPaused = await protocolConfig.read.paused()
        const oldFeeParams = await protocolConfig.read.feeParams()

        const initData = encodeFunctionData({
          abi: parseAbi(['function initialize(address,address,bool)']),
          functionName: 'initialize',
          args: [oldController, oldPauser, oldPaused],
        })

        const newFeeParams = {
          alpha: oldFeeParams.alpha + 1n,
          kRate: oldFeeParams.kRate + 1n,
          inverseElasticityMultiplier: oldFeeParams.inverseElasticityMultiplier + 1n,
          minBaseFee: oldFeeParams.minBaseFee + 1n,
          maxBaseFee: oldFeeParams.maxBaseFee + 1n,
          blockGasLimit: oldFeeParams.blockGasLimit + 1n,
        }

        const result = await client.simulateBlocks({
          blocks: [
            {
              calls: [
                {
                  account: admin,
                  to: protocolConfig.address,
                  abi: parseAbi(['function upgradeToAndCall(address,bytes)']),
                  functionName: 'upgradeToAndCall',
                  args: [newImplementation, initData],
                },
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'updateFeeParams',
                  args: [newFeeParams],
                },
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'feeParams',
                  args: [],
                },
              ],
            },
          ],
        })

        const calls = result[0].calls
        expect(calls[0].status).to.equal('success')
        expect(calls[1].status).to.equal('success')
        expect(calls[2].status).to.equal('success')

        const returnedFeeParams = decodeFunctionResult({
          abi: protocolConfig.abi,
          functionName: 'feeParams',
          data: calls[2].data,
        })
        expect(returnedFeeParams.alpha).to.equal(newFeeParams.alpha)
        expect(returnedFeeParams.blockGasLimit).to.equal(newFeeParams.blockGasLimit)
      })
    },
  )
})
