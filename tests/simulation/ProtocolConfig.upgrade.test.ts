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

/* eslint-disable @typescript-eslint/no-unsafe-member-access */
/* eslint-disable @typescript-eslint/no-explicit-any */

import { expect } from 'chai'
import hre from 'hardhat'
import { getChain } from '../../scripts/hardhat/viem-helper'
import { ProtocolConfig } from '../helpers'
import { loadGenesisConfig } from '../helpers'
import { parseAbi, Address, decodeFunctionResult, Hex } from 'viem'
import { privateKeyToAccount, generatePrivateKey } from 'viem/accounts'
import fs from 'fs'

/**
 * Simulation tests for ProtocolConfig upgrade
 * Requires NEW_IMPLEMENTATION_ADDRESS environment variable
 *
 * Usage:
 *   NEW_IMPLEMENTATION_ADDRESS=0x... npx hardhat test tests/simulation/ProtocolConfig.upgrade.test.ts --network testnet
 */
describe('ProtocolConfig upgrade simulation', () => {
  const genesisConfig = loadGenesisConfig()
  if (!genesisConfig) {
    console.log('Skipping - genesis config not loaded')
    return
  }

  // ============ Test Setup ============

  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const protocolConfig = ProtocolConfig.attach(client)

    const upgradeAbi = parseAbi([
      'function upgradeTo(address newImplementation)',
      'function upgradeToAndCall(address newImplementation, bytes memory data)',
      'function implementation() view returns (address)',
      'function admin() view returns (address)',
    ])

    return { client, protocolConfig, upgradeAbi }
  }

  /**
   * Get new implementation address from environment
   */
  const getNewImplementation = () => process.env.NEW_IMPLEMENTATION_ADDRESS as Address | undefined
  const newImplementation = getNewImplementation() as Address
  ;(newImplementation && genesisConfig?.ProtocolConfig?.proxy?.admin ? describe : describe.skip)(
    'Upgrade simulation',
    () => {
      it('should simulate upgradeTo successfully', async () => {
        const { client, protocolConfig } = await clients()
        const proxyAdmin = genesisConfig.ProtocolConfig.proxy.admin

        // Simulate upgrade
        const result = await client.simulateCalls({
          account: proxyAdmin,
          calls: [
            {
              to: protocolConfig.address,
              abi: parseAbi(['function upgradeTo(address)']),
              functionName: 'upgradeTo',
              args: [newImplementation],
            },
          ],
        })

        expect(result.results[0].status).to.equal('success', 'Upgrade simulation failed')
      })

      it('should verify new implementation has correct bytecode', async () => {
        const { client } = await clients()

        // Get deployed bytecode
        const deployedCode = await client.getCode({ address: newImplementation })
        expect(deployedCode).to.not.be.undefined
        expect(deployedCode).to.not.equal('0x')

        // Get Forge compiled bytecode from artifact
        const forgeArtifactPath = 'contracts/out/forge/ProtocolConfig.sol/ProtocolConfig.json'
        const forgeArtifact = JSON.parse(fs.readFileSync(forgeArtifactPath, 'utf8')) as {
          deployedBytecode: { object: Hex }
        }
        const forgeBytecode = forgeArtifact.deployedBytecode.object

        // Compare bytecode (should match exactly since both use Forge)
        expect(deployedCode).to.equal(forgeBytecode, 'Deployed bytecode does not match Forge compilation')
      })

      it('should simulate upgrade with config updates in same block', async () => {
        const { client, protocolConfig } = await clients()

        if (!genesisConfig.ProtocolConfig?.controller) {
          return
        }

        const proxyAdmin = genesisConfig.ProtocolConfig.proxy.admin
        const controller = genesisConfig.ProtocolConfig.controller

        // Get current params
        const currentFeeParams = await protocolConfig.read.feeParams()
        const currentConsensusParams = await protocolConfig.read.consensusParams()

        // New params for testing (all numeric values +1)
        const newFeeParams = {
          alpha: currentFeeParams.alpha + 1n,
          kRate: currentFeeParams.kRate + 1n,
          inverseElasticityMultiplier: currentFeeParams.inverseElasticityMultiplier + 1n,
          minBaseFee: currentFeeParams.minBaseFee + 1n,
          maxBaseFee: currentFeeParams.maxBaseFee + 1n,
          blockGasLimit: currentFeeParams.blockGasLimit + 1n,
        }

        const newConsensusParams = {
          timeoutProposeMs: currentConsensusParams.timeoutProposeMs + 1,
          timeoutProposeDeltaMs: currentConsensusParams.timeoutProposeDeltaMs + 1,
          timeoutPrevoteMs: currentConsensusParams.timeoutPrevoteMs + 1,
          timeoutPrevoteDeltaMs: currentConsensusParams.timeoutPrevoteDeltaMs + 1,
          timeoutPrecommitMs: currentConsensusParams.timeoutPrecommitMs + 1,
          timeoutPrecommitDeltaMs: currentConsensusParams.timeoutPrecommitDeltaMs + 1,
          timeoutRebroadcastMs: currentConsensusParams.timeoutRebroadcastMs + 1,
          targetBlockTimeMs: currentConsensusParams.targetBlockTimeMs + 1,
        }

        // Generate a random address for new beneficiary
        const randomAccount = privateKeyToAccount(generatePrivateKey())
        const newBeneficiary = randomAccount.address

        // Simulate: Upgrade + Update FeeParams + Update ConsensusParams + Update Beneficiary + Read to verify
        const result = await client.simulateBlocks({
          blocks: [
            {
              calls: [
                // 1. Upgrade
                {
                  account: proxyAdmin,
                  to: protocolConfig.address,
                  abi: parseAbi(['function upgradeTo(address)']),
                  functionName: 'upgradeTo',
                  args: [newImplementation],
                },
                // 2. Update fee params (verify write works after upgrade)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'updateFeeParams',
                  args: [newFeeParams],
                },
                // 3. Update consensus params (verify another write works)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'updateConsensusParams',
                  args: [newConsensusParams],
                },
                // 4. Update reward beneficiary (verify beneficiary update works)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'updateRewardBeneficiary',
                  args: [newBeneficiary],
                },
                // 5. Read fee params back (verify read works)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'feeParams',
                  args: [],
                },
                // 6. Read consensus params back (verify read works)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'consensusParams',
                  args: [],
                },
                // 7. Read reward beneficiary back (verify read works)
                {
                  account: controller,
                  to: protocolConfig.address,
                  abi: protocolConfig.abi,
                  functionName: 'rewardBeneficiary',
                  args: [],
                },
              ],
            },
          ],
        })

        // Verify all calls succeeded
        const calls = result[0].calls
        expect(calls[0].status).to.equal('success', 'Upgrade failed')
        expect(calls[1].status).to.equal('success', 'Update fee params failed after upgrade')
        expect(calls[2].status).to.equal('success', 'Update consensus params failed after upgrade')
        expect(calls[3].status).to.equal('success', 'Update reward beneficiary failed after upgrade')
        expect(calls[4].status).to.equal('success', 'Read fee params failed after upgrade')
        expect(calls[5].status).to.equal('success', 'Read consensus params failed after upgrade')
        expect(calls[6].status).to.equal('success', 'Read reward beneficiary failed after upgrade')

        // Verify the returned values match the new params
        const returnedFeeParams: any = decodeFunctionResult({
          abi: protocolConfig.abi,
          functionName: 'feeParams',
          data: calls[4].data,
        })

        const returnedConsensusParams: any = decodeFunctionResult({
          abi: protocolConfig.abi,
          functionName: 'consensusParams',
          data: calls[5].data,
        })

        const returnedBeneficiary: any = decodeFunctionResult({
          abi: protocolConfig.abi,
          functionName: 'rewardBeneficiary',
          data: calls[6].data,
        })

        // Verify fee params were updated
        expect(returnedFeeParams.alpha).to.equal(newFeeParams.alpha, 'Alpha not updated')
        expect(returnedFeeParams.kRate).to.equal(newFeeParams.kRate, 'kRate not updated')
        expect(returnedFeeParams.inverseElasticityMultiplier).to.equal(
          newFeeParams.inverseElasticityMultiplier,
          'InverseElasticityMultiplier not updated',
        )
        expect(returnedFeeParams.minBaseFee).to.equal(newFeeParams.minBaseFee, 'MinBaseFee not updated')
        expect(returnedFeeParams.maxBaseFee).to.equal(newFeeParams.maxBaseFee, 'MaxBaseFee not updated')
        expect(returnedFeeParams.blockGasLimit).to.equal(newFeeParams.blockGasLimit, 'BlockGasLimit not updated')

        // Verify consensus params were updated
        expect(returnedConsensusParams.timeoutProposeMs).to.equal(
          newConsensusParams.timeoutProposeMs,
          'timeoutProposeMs not updated',
        )
        expect(returnedConsensusParams.timeoutProposeDeltaMs).to.equal(
          newConsensusParams.timeoutProposeDeltaMs,
          'timeoutProposeDeltaMs not updated',
        )
        expect(returnedConsensusParams.timeoutPrevoteMs).to.equal(
          newConsensusParams.timeoutPrevoteMs,
          'timeoutPrevoteMs not updated',
        )
        expect(returnedConsensusParams.timeoutPrevoteDeltaMs).to.equal(
          newConsensusParams.timeoutPrevoteDeltaMs,
          'timeoutPrevoteDeltaMs not updated',
        )
        expect(returnedConsensusParams.timeoutPrecommitMs).to.equal(
          newConsensusParams.timeoutPrecommitMs,
          'timeoutPrecommitMs not updated',
        )
        expect(returnedConsensusParams.timeoutPrecommitDeltaMs).to.equal(
          newConsensusParams.timeoutPrecommitDeltaMs,
          'timeoutPrecommitDeltaMs not updated',
        )
        expect(returnedConsensusParams.timeoutRebroadcastMs).to.equal(
          newConsensusParams.timeoutRebroadcastMs,
          'timeoutRebroadcastMs not updated',
        )
        expect(returnedConsensusParams.targetBlockTimeMs).to.equal(
          newConsensusParams.targetBlockTimeMs,
          'targetBlockTimeMs not updated',
        )

        // Verify reward beneficiary was updated
        expect(returnedBeneficiary).to.equal(newBeneficiary, 'Reward beneficiary not updated')
      })
    },
  )
})
