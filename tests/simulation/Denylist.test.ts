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
import { Denylist, loadGenesisConfig } from '../helpers'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { decodeFunctionResult, encodeFunctionData, parseAbi, zeroAddress } from 'viem'
import { multicall3Address } from '../../scripts/genesis'

describe('Denylist simulation', () => {
  const genesisConfig = loadGenesisConfig()
  const denylistGenesis = genesisConfig?.Denylist

  const clients = async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const denylist = Denylist.attach(client)
    const randomWallet = privateKeyToAccount(generatePrivateKey())
    return { client, denylist, randomWallet }
  }

  it('contract exists and is initialized', async () => {
    const { client, denylist } = await clients()
    const code = await client.getCode({ address: Denylist.address })
    expect(code?.length).to.be.greaterThan(0)

    const owner = await denylist.read.owner()
    expect(owner).to.not.eq(zeroAddress)
  })

  // Scenario: proxy admin can upgrade the implementation.
  // Flow: proxyAdmin → Denylist proxy.upgradeTo(multicall3Address)
  // Assertions: simulateCalls returns success.
  it('proxy admin can upgrade the contract', async () => {
    const { client, denylist } = await clients()
    const proxyAdmin = await denylist.read.admin()
    expect(proxyAdmin).to.not.eq(zeroAddress)

    const result = await client.simulateCalls({
      account: proxyAdmin,
      calls: [
        {
          to: Denylist.address,
          data: encodeFunctionData({
            abi: parseAbi(['function upgradeTo(address)']),
            functionName: 'upgradeTo',
            args: [multicall3Address],
          }),
        },
      ],
    })
    expect(result.results[0].status).to.eq('success')
  })

  // Scenario: non-admin cannot upgrade.
  // Flow: randomWallet → Denylist proxy.upgradeTo(...)
  // Assertions: simulateCalls returns failure.
  it('non-admin cannot upgrade the contract', async () => {
    const { client, randomWallet } = await clients()

    const result = await client.simulateCalls({
      account: randomWallet.address,
      calls: [
        {
          to: Denylist.address,
          data: encodeFunctionData({
            abi: parseAbi(['function upgradeTo(address)']),
            functionName: 'upgradeTo',
            args: [multicall3Address],
          }),
        },
      ],
    })
    expect(result.results[0].status).to.eq('failure')
  })

  // Scenario: owner can add a denylister, then the new denylister can denylist an address.
  // Flow: owner → addDenylister(randomWallet) → randomWallet → denylist([target]) → isDenylisted(target)
  // Assertions: all calls succeed, isDenylisted returns true.
  it('owner can add denylister, denylister can denylist', async () => {
    const { client, denylist, randomWallet } = await clients()
    const owner = await denylist.read.owner()
    const target = privateKeyToAccount(generatePrivateKey())

    const result = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            // owner adds randomWallet as denylister
            {
              account: owner,
              to: Denylist.address,
              data: encodeFunctionData({
                abi: denylist.abi,
                functionName: 'addDenylister',
                args: [randomWallet.address],
              }),
            },
            // new denylister adds target to denylist
            {
              account: randomWallet.address,
              to: Denylist.address,
              data: encodeFunctionData({
                abi: denylist.abi,
                functionName: 'denylist',
                args: [[target.address]],
              }),
            },
            // verify target is denylisted
            {
              account: owner,
              to: Denylist.address,
              data: encodeFunctionData({
                abi: denylist.abi,
                functionName: 'isDenylisted',
                args: [target.address],
              }),
            },
          ],
        },
      ],
    })

    const calls = result[0].calls
    expect(calls[0].status).to.eq('success', 'addDenylister should succeed')
    expect(calls[1].status).to.eq('success', 'denylist should succeed')
    expect(calls[2].status).to.eq('success', 'isDenylisted read should succeed')

    const isDenylisted = decodeFunctionResult({
      abi: denylist.abi,
      functionName: 'isDenylisted',
      data: calls[2].data,
    })
    expect(isDenylisted).to.eq(true)
  })

  // Scenario: non-denylister cannot denylist.
  // Flow: randomWallet (not a denylister) → denylist([target])
  // Assertions: call fails with CallerIsNotDenylister.
  it('non-denylister cannot denylist', async () => {
    const { client, denylist, randomWallet } = await clients()
    const target = privateKeyToAccount(generatePrivateKey())

    const result = await client.simulateCalls({
      account: randomWallet.address,
      calls: [
        {
          to: Denylist.address,
          data: encodeFunctionData({
            abi: denylist.abi,
            functionName: 'denylist',
            args: [[target.address]],
          }),
        },
      ],
    })
    expect(result.results[0].status).to.eq('failure')
  })

  // Scenario: non-owner cannot add denylister.
  // Flow: randomWallet (not owner) → addDenylister(target)
  // Assertions: call fails.
  it('non-owner cannot add denylister', async () => {
    const { client, denylist, randomWallet } = await clients()
    const target = privateKeyToAccount(generatePrivateKey())

    const result = await client.simulateCalls({
      account: randomWallet.address,
      calls: [
        {
          to: Denylist.address,
          data: encodeFunctionData({
            abi: denylist.abi,
            functionName: 'addDenylister',
            args: [target.address],
          }),
        },
      ],
    })
    expect(result.results[0].status).to.eq('failure')
  })

  // Scenario: denylister cannot denylist the owner.
  // Flow: owner → addDenylister(denylister) → denylister → denylist([owner])
  // Assertions: addDenylister succeeds, denylist([owner]) fails with CannotDenylistOwner.
  it('denylister cannot denylist the owner', async () => {
    const { client, denylist, randomWallet } = await clients()
    const owner = await denylist.read.owner()

    const result = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              account: owner,
              to: Denylist.address,
              data: encodeFunctionData({
                abi: denylist.abi,
                functionName: 'addDenylister',
                args: [randomWallet.address],
              }),
            },
            {
              account: randomWallet.address,
              to: Denylist.address,
              data: encodeFunctionData({
                abi: denylist.abi,
                functionName: 'denylist',
                args: [[owner]],
              }),
            },
          ],
        },
      ],
    })

    expect(result[0].calls[0].status).to.eq('success', 'addDenylister should succeed')
    expect(result[0].calls[1].status).to.eq('failure', 'denylist owner should fail')
  })

  // Scenario: genesis denylister wallet (if configured) can denylist.
  // Flow: genesis denylister → denylist([target]) → isDenylisted(target)
  // Assertions: all calls succeed, isDenylisted returns true.
  ;(denylistGenesis?.denylisters?.length ? describe : describe.skip)('genesis denylister role validation', () => {
    it('genesis denylister can denylist an address', async () => {
      const denylisterWallet = denylistGenesis!.denylisters![0]
      const { client, denylist } = await clients()
      const target = privateKeyToAccount(generatePrivateKey())

      const onchainIsDenylister = await denylist.read.isDenylister([denylisterWallet])
      expect(onchainIsDenylister).to.eq(true, 'genesis denylister should be a denylister on-chain')

      const result = await client.simulateBlocks({
        blocks: [
          {
            calls: [
              {
                account: denylisterWallet,
                to: Denylist.address,
                data: encodeFunctionData({
                  abi: denylist.abi,
                  functionName: 'denylist',
                  args: [[target.address]],
                }),
              },
              {
                account: denylisterWallet,
                to: Denylist.address,
                data: encodeFunctionData({
                  abi: denylist.abi,
                  functionName: 'isDenylisted',
                  args: [target.address],
                }),
              },
            ],
          },
        ],
      })

      expect(result[0].calls[0].status).to.eq('success', 'denylist call should succeed')
      expect(result[0].calls[1].status).to.eq('success', 'isDenylisted read should succeed')

      const isDenylisted = decodeFunctionResult({
        abi: denylist.abi,
        functionName: 'isDenylisted',
        data: result[0].calls[1].data,
      })
      expect(isDenylisted).to.eq(true)
    })
  })
})
