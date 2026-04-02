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
import { USDC } from '../helpers/FiatToken'
import { loadGenesisConfig } from '../helpers'
import { parseAbi, Address } from 'viem'
import { fiatTokenProxyAddress } from '../../scripts/genesis'

/**
 * Simulation tests for NativeFiatToken upgrade
 * Requires NEW_IMPLEMENTATION_ADDRESS environment variable
 */
describe('NativeFiatToken upgrade simulation', () => {
  const genesisConfig = loadGenesisConfig()
  if (!genesisConfig) {
    console.log('Skipping - genesis config not loaded')
    return
  }
  const PROXY_ADDRESS = fiatTokenProxyAddress

  // ============ Test Setup ============

  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const usdc = USDC.attach(client)

    const upgradeAbi = parseAbi([
      'function upgradeTo(address newImplementation)',
      'function implementation() view returns (address)',
      'function admin() view returns (address)',
    ])

    return { client, usdc, upgradeAbi }
  }

  /**
   * Get new implementation address from environment
   */
  const getNewImplementation = () => process.env.NEW_IMPLEMENTATION_ADDRESS as Address | undefined
  const newImplementation = getNewImplementation() as Address
  ;(newImplementation && genesisConfig?.NativeFiatToken?.proxy?.admin ? describe : describe.skip)(
    'Upgrade simulation',
    () => {
      it('should simulate upgrade successfully', async () => {
        const { client } = await clients()
        const proxyAdmin = genesisConfig.NativeFiatToken.proxy.admin

        // Simulate upgrade
        const result = await client.simulateCalls({
          account: proxyAdmin,
          calls: [
            {
              to: PROXY_ADDRESS,
              abi: parseAbi(['function upgradeTo(address)']),
              functionName: 'upgradeTo',
              args: [newImplementation],
            },
          ],
        })

        expect(result.results[0].status).to.equal('success')
        console.log('Upgrade simulation succeeded')
      })

      it('should simulate upgrade with transactions after', async () => {
        const { client, usdc } = await clients()

        if (!genesisConfig.NativeFiatToken.minters?.[0]) {
          console.log('Missing minter in genesis')
          return
        }

        const proxyAdmin = genesisConfig.NativeFiatToken.proxy.admin
        const minter = genesisConfig.NativeFiatToken.minters[0]

        // Simulate: Upgrade + Mint + Transfer
        const result = await client.simulateBlocks({
          blocks: [
            {
              calls: [
                // 1. Upgrade
                {
                  account: proxyAdmin,
                  to: PROXY_ADDRESS,
                  abi: parseAbi(['function upgradeTo(address)']),
                  functionName: 'upgradeTo',
                  args: [newImplementation],
                },
                // 2. Mint (verifies mint works after upgrade)
                {
                  account: minter.address,
                  to: USDC.address,
                  abi: usdc.abi,
                  functionName: 'mint',
                  args: [minter.address, USDC.parseUnits('10')],
                },
                // 3. Transfer (verifies transfer works after upgrade)
                {
                  account: minter.address,
                  to: USDC.address,
                  abi: usdc.abi,
                  functionName: 'transfer',
                  args: [proxyAdmin, USDC.parseUnits('5')],
                },
              ],
            },
          ],
        })

        // simulateBlocks returns array of blocks directly
        const calls = result[0].calls
        expect(calls[0].status).to.equal('success', 'Upgrade failed')
        expect(calls[1].status).to.equal('success', 'Mint failed after upgrade')
        expect(calls[2].status).to.equal('success', 'Transfer failed after upgrade')

        console.log('Upgrade + Mint + Transfer simulation passed')
      })
    },
  )
})
