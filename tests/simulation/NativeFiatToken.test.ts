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
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { Address, concat, InternalRpcError, parseAbi, parseEther, toHex, zeroAddress } from 'viem'
import type { Call } from 'viem/types/calls'
import { multicall3Address, fiatTokenProxyAddress } from '../../scripts/genesis'
import { loadGenesisConfig } from '../helpers'
import { multicall3Abi } from '../helpers/CallHelper'
import { ERR_BLOCKED_ADDRESS } from '../helpers/NativeCoinControl'

describe('NativeFiatToken simulation', () => {
  const genesisConfig = loadGenesisConfig()
  const PROXY_ADDRESS = fiatTokenProxyAddress

  const mockBalances = (...addresses: Array<Address>) =>
    addresses.filter((x, i) => addresses.indexOf(x) === i).map((address) => ({ address, balance: parseEther('100') }))

  /**
   * Get new implementation address from environment
   */
  const getNewImplementation = () => process.env.NEW_IMPLEMENTATION_ADDRESS as Address | undefined

  /**
   * Returns upgrade call if NEW_IMPLEMENTATION_ADDRESS is set, empty array otherwise
   */
  const maybeUpgrade = () => {
    const newImpl = getNewImplementation()
    const admin = genesisConfig?.NativeFiatToken?.proxy?.admin

    if (newImpl && admin) {
      return [
        {
          account: admin,
          to: PROXY_ADDRESS,
          abi: parseAbi(['function upgradeTo(address)']),
          functionName: 'upgradeTo',
          args: [newImpl],
        },
      ]
    }
    return []
  }

  const getCallIndexOffset = () => maybeUpgrade().length

  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const usdc = USDC.attach(client)
    const randomWallet = privateKeyToAccount(generatePrivateKey())
    const extraAbi = parseAbi([
      'function upgradeTo(address newImplementation)',
      'function initializeV2(string calldata newName)',
      'function initializeV2_1(address lostAndFound)',
      'function initializeV2_2(address[] accountsToBlacklist, string calldata newSymbol)',
    ])

    return { client, usdc, newAddress: randomWallet.address, extraAbi }
  }

  describe('mint', () => {
    it('add minter by masterMinter', async () => {
      const { client, usdc, newAddress } = await clients()
      const masterMinter = await usdc.read.masterMinter()
      expect(masterMinter).to.not.eq(zeroAddress)
      const res = await client.simulateCalls({
        account: masterMinter,
        calls: [{ to: USDC.address, abi: usdc.abi, functionName: 'configureMinter', args: [newAddress, 1n] }],
      })
      expect(res.results[0].status).to.be.eq('success')
    })

    if (genesisConfig?.NativeFiatToken.minters[0] != null) {
      it('mint by configured minter', async () => {
        const { client, usdc, newAddress } = await clients()
        for (const minter of genesisConfig?.NativeFiatToken.minters ?? []) {
          expect(minter.address).to.not.eq(zeroAddress)
          const isMinter = await usdc.read.isMinter([minter.address])
          if (!isMinter) {
            continue
          }
          const res = await client.simulateCalls({
            account: minter.address,
            calls: [{ to: USDC.address, abi: usdc.abi, functionName: 'mint', args: [newAddress, 1n] }],
          })
          expect(res.results[0].status).to.be.eq('success')
          break
        }
      })
    }

    it('add minter to mint and burn', async () => {
      const { client, usdc, newAddress } = await clients()
      const [masterMinter, pauser] = await Promise.all([usdc.read.masterMinter(), usdc.read.pauser()])
      expect(masterMinter).to.not.eq(zeroAddress)
      expect(pauser).to.not.eq(zeroAddress)
      const amount = 7n
      const allowance = amount * 4n
      const mintAndBurn = [
        { account: newAddress, to: USDC.address, abi: usdc.abi, functionName: 'mint', args: [newAddress, amount] },
        { account: newAddress, to: USDC.address, abi: usdc.abi, functionName: 'burn', args: [amount] },
      ] as const

      const res = await client.simulateBlocks({
        blocks: [
          {
            calls: [
              ...maybeUpgrade(),
              {
                account: masterMinter,
                to: USDC.address,
                abi: usdc.abi,
                functionName: 'configureMinter',
                args: [newAddress, allowance],
              },
              ...mintAndBurn,
              { account: pauser, to: USDC.address, abi: usdc.abi, functionName: 'pause', args: [] },
              ...mintAndBurn,
              { account: pauser, to: USDC.address, abi: usdc.abi, functionName: 'unpause', args: [] },
              ...mintAndBurn,
              {
                account: masterMinter,
                to: USDC.address,
                abi: usdc.abi,
                functionName: 'removeMinter',
                args: [newAddress],
              },
              ...mintAndBurn,
            ],
            blockOverrides: { baseFeePerGas: 10n },
            stateOverrides: mockBalances(newAddress, pauser, masterMinter),
          },
        ],
      })

      let i = getCallIndexOffset()
      expect(res[0].calls[i++].status).to.be.eq('success') // configureMinter
      expect(res[0].calls[i++].status).to.be.eq('success')
      expect(res[0].calls[i++].status).to.be.eq('success')
      expect(res[0].calls[i++].status).to.be.eq('success') // pause
      expect(res[0].calls[i++].status).to.be.eq('failure')
      expect(res[0].calls[i++].status).to.be.eq('failure')
      expect(res[0].calls[i++].status).to.be.eq('success') // unpause
      expect(res[0].calls[i++].status).to.be.eq('success')
      expect(res[0].calls[i++].status).to.be.eq('success')
      expect(res[0].calls[i++].status).to.be.eq('success') // removeMinter
      expect(res[0].calls[i++].status).to.be.eq('failure')
      expect(res[0].calls[i++].status).to.be.eq('failure')
    })
  })

  describe('Blacklistable', () => {
    it('blacklist by blacklister should be blocked', async () => {
      const { client, usdc, newAddress } = await clients()
      const blacklister = await usdc.read.blacklister()
      expect(blacklister).to.not.eq(zeroAddress)

      const res = await client.simulateCalls({
        account: blacklister,
        calls: [
          { to: USDC.address, abi: usdc.abi, functionName: 'blacklist', args: [newAddress] },
          {
            to: multicall3Address,
            value: 7n,
            abi: multicall3Abi,
            functionName: 'aggregate3Value',
            args: [[{ target: newAddress, allowFailure: false, value: 7n, callData: '0x' }]],
          },
          { to: USDC.address, abi: usdc.abi, functionName: 'transfer', args: [newAddress, 1n] },
          { to: USDC.address, abi: usdc.abi, functionName: 'unBlacklist', args: [newAddress] },
          {
            to: multicall3Address,
            value: 7n,
            abi: multicall3Abi,
            functionName: 'aggregate3Value',
            args: [[{ target: newAddress, allowFailure: false, value: 7n, callData: '0x' }]],
          },
          { to: USDC.address, abi: usdc.abi, functionName: 'transfer', args: [newAddress, 1n] },
        ],
        stateOverrides: mockBalances(blacklister, newAddress),
      })
      let i = 0
      expect(res.results[i++].status).to.be.eq('success') // blacklist
      expect(res.results[i++].status).to.be.eq('failure') // internal transfer
      expect(res.results[i++].status).to.be.eq('failure') // transfer ERC20
      expect(res.results[i++].status).to.be.eq('success') // unBlacklist
      expect(res.results[i++].status).to.be.eq('success') // internal transfer
      expect(res.results[i++].status).to.be.eq('success') // transfer ERC20
    })

    it('send to blocked address should be rejected in pre-execution stage', async () => {
      const { client, usdc, newAddress } = await clients()
      const blacklister = await usdc.read.blacklister()
      await expect(
        client.simulateCalls({
          account: blacklister,
          calls: [
            { to: USDC.address, abi: usdc.abi, functionName: 'blacklist', args: [newAddress] },
            { to: newAddress, value: 7n },
          ],
        }),
      ).to.be.rejectedWith(InternalRpcError, ERR_BLOCKED_ADDRESS)
    })

    it('send to blocked address should be rejected in ERC20, and propagate the error', async () => {
      const { client, usdc, newAddress } = await clients()
      const blacklister = await usdc.read.blacklister()
      const gas = 60000n
      const res = await client.simulateCalls({
        account: blacklister,
        calls: [
          { to: USDC.address, abi: usdc.abi, functionName: 'blacklist', args: [newAddress] },
          { to: USDC.address, abi: usdc.abi, functionName: 'transfer', args: [newAddress, 1n], gas } as Call,
        ],
      })
      expect(res.results[0].status).to.be.eq('success')
      expect(res.results[1].status).to.be.eq('failure')
      expect(res.results[1].gasUsed).to.be.lessThan(gas)
      expect(res.results[1].error?.message).to.contain('execution reverted: Blocked address')
    })

    it('transferFrom should reject when FROM address is blacklisted', async () => {
      const { client, usdc, newAddress } = await clients()
      const blacklister = await usdc.read.blacklister()
      const gas = 100000n
      const amount = 100n
      const randomWallet = privateKeyToAccount(generatePrivateKey())
      const spender = privateKeyToAccount(generatePrivateKey())

      await expect(
        client.simulateBlocks({
          blocks: [
            {
              calls: [
                // Blacklist the FROM address (newAddress)
                { to: USDC.address, abi: usdc.abi, functionName: 'blacklist', args: [newAddress], from: blacklister },
                // This call will trigger pre-execution blocker
                {
                  to: USDC.address,
                  abi: usdc.abi,
                  functionName: 'transferFrom',
                  args: [spender.address, randomWallet.address, amount],
                  from: newAddress,
                  gas,
                } as Call,
              ],
            },
          ],
        }),
      ).to.be.rejectedWith(InternalRpcError, ERR_BLOCKED_ADDRESS)
    })

    it('update blacklister', async () => {
      const { client, usdc, newAddress } = await clients()
      const owner = await usdc.read.owner()
      const res = await client.simulateCalls({
        account: owner,
        calls: [{ to: USDC.address, abi: usdc.abi, functionName: 'updateBlacklister', args: [newAddress] }],
      })
      expect(res.results[0].status).to.be.eq('success')
    })
  })

  it('change owner', async () => {
    const { client, usdc, newAddress } = await clients()
    const owner = await usdc.read.owner()
    expect(owner).to.not.eq(zeroAddress)
    const res = await client.simulateCalls({
      account: owner,
      calls: [{ to: USDC.address, abi: usdc.abi, functionName: 'transferOwnership', args: [newAddress] }],
    })
    expect(res.results[0].status).to.be.eq('success')
  })

  it('rescue', async () => {
    const { client, usdc, newAddress } = await clients()
    const [owner, rescuer] = await Promise.all([usdc.read.owner(), usdc.read.rescuer()])
    const someAddress = toHex(1n, { size: 20 })
    expect(owner).to.not.eq(zeroAddress)
    expect(rescuer).to.not.eq(zeroAddress)
    const amountERC20 = USDC.parseUnits('100')
    const amount = USDC.toNative(amountERC20)

    const transferERC20 = (from: Address, to: Address, amount: bigint) =>
      ({ account: from, to: USDC.address, abi: usdc.abi, functionName: 'transfer', args: [to, amount] }) as const

    const res = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            ...maybeUpgrade(),
            transferERC20(newAddress, USDC.address, amountERC20),
            {
              account: rescuer,
              to: USDC.address,
              abi: usdc.abi,
              functionName: 'rescueERC20',
              args: [USDC.address, someAddress, amountERC20],
            },
            transferERC20(someAddress, USDC.address, amountERC20),
            { account: owner, to: USDC.address, abi: usdc.abi, functionName: 'updateRescuer', args: [newAddress] },
            {
              account: rescuer,
              to: USDC.address,
              abi: usdc.abi,
              functionName: 'rescueERC20',
              args: [USDC.address, someAddress, amountERC20],
            },
            transferERC20(someAddress, USDC.address, amountERC20),
            {
              account: newAddress,
              to: USDC.address,
              abi: usdc.abi,
              functionName: 'rescueERC20',
              args: [USDC.address, someAddress, amountERC20],
            },
            transferERC20(someAddress, USDC.address, amountERC20),
          ],
          stateOverrides: [
            { address: newAddress, balance: amount + parseEther('0.01') },
            { address: someAddress, balance: parseEther('0.01') },
          ],
        },
      ],
    })
    let i = getCallIndexOffset()
    expect(res[0].calls[i++].status).to.be.eq('success') // newAddress -> USDC
    expect(res[0].calls[i++].status).to.be.eq('success') // rescueERC20: USDC -> someAddress
    expect(res[0].calls[i++].status).to.be.eq('success') // someAddress -> USDC
    expect(res[0].calls[i++].status).to.be.eq('success') // updateRescuer
    expect(res[0].calls[i++].status).to.be.eq('failure') // rescueERC20 by old rescuer
    expect(res[0].calls[i++].status).to.be.eq('failure') // someAddress -> USDC
    expect(res[0].calls[i++].status).to.be.eq('success') // rescueERC20: USDC -> someAddress
    expect(res[0].calls[i++].status).to.be.eq('success') // someAddress -> USDC
  })

  it('migrate contract', async () => {
    const { client, usdc, extraAbi } = await clients()
    const proxyAdmin = await usdc.read.admin()
    expect(proxyAdmin).to.not.eq(zeroAddress)
    const res = await client.simulateCalls({
      account: proxyAdmin,
      calls: [{ to: USDC.address, abi: extraAbi, functionName: 'upgradeTo', args: [multicall3Address] }],
    })
    expect(res.results[0].status).to.be.eq('success')
  })

  it('initial slot', async () => {
    const { client, usdc } = await clients()
    const [masterMinter, inited, version] = await Promise.all([
      usdc.read.masterMinter(),
      client.getStorageAt({ address: USDC.address, slot: toHex(8n, { size: 32 }) }),
      client.getStorageAt({ address: USDC.address, slot: toHex(18n, { size: 32 }) }),
    ])

    expect(inited).to.be.eq(concat([toHex(1n, { size: 12 }), masterMinter.toLowerCase() as Address]))
    expect(version).to.be.eq(toHex(3n, { size: 32 }))
  })

  it('no permission from random wallet', async () => {
    const { client, usdc, newAddress, extraAbi } = await clients()
    const someAddress = toHex(1n, { size: 20 })
    const initAbi = parseAbi(['function initialize(string,string,string,uint8,address,address,address,address)'])

    // Verify contract is at version 3 (fully initialized)
    const version = await client.getStorageAt({ address: USDC.address, slot: toHex(18n, { size: 32 }) })
    expect(version).to.be.eq(toHex(3n, { size: 32 }))

    const res = await client.simulateCalls({
      account: newAddress,
      calls: [
        { to: USDC.address, abi: usdc.abi, functionName: 'configureMinter', args: [someAddress, 1n] },
        { to: USDC.address, abi: usdc.abi, functionName: 'blacklist', args: [someAddress] },
        { to: USDC.address, abi: usdc.abi, functionName: 'transferOwnership', args: [someAddress] },
        { to: USDC.address, abi: usdc.abi, functionName: 'updateRescuer', args: [someAddress] },
        {
          to: USDC.address,
          abi: initAbi,
          functionName: 'initialize',
          args: ['USDC', 'USDC', 'USD', 6, someAddress, someAddress, someAddress, someAddress],
        },
        { to: USDC.address, abi: extraAbi, functionName: 'initializeV2', args: ['USDCx'] },
        { to: USDC.address, abi: extraAbi, functionName: 'initializeV2_1', args: [someAddress] },
        { to: USDC.address, abi: extraAbi, functionName: 'initializeV2_2', args: [[someAddress], 'USDCx'] },
        {
          to: USDC.address,
          abi: parseAbi(['function upgradeTo(address newImplementation)']),
          functionName: 'upgradeTo',
          args: [multicall3Address],
        },
      ],
      stateOverrides: mockBalances(newAddress),
    })
    let i = 0
    expect(res.results[i++].status).to.be.eq('failure') // configureMinter
    expect(res.results[i++].status).to.be.eq('failure') // blacklist
    expect(res.results[i++].status).to.be.eq('failure') // transferOwnership
    expect(res.results[i++].status).to.be.eq('failure') // updateRescuer
    expect(res.results[i++].status).to.be.eq('failure') // initialize
    expect(res.results[i++].status).to.be.eq('failure') // initializeV2
    expect(res.results[i++].status).to.be.eq('failure') // initializeV2_1
    expect(res.results[i++].status).to.be.eq('failure') // initializeV2_2
    expect(res.results[i++].status).to.be.eq('failure') // upgradeTo
  })
})
