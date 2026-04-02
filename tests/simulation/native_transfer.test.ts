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
import { encodeFunctionData, parseEther, parseEventLogs } from 'viem'
import { getChain } from '../../scripts/hardhat/viem-helper'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { NativeCoinAuthority } from '../helpers'

/**
 * Tests for native token transfers
 */
describe('Native Transfer Tests', () => {
  const randomAddress = () => privateKeyToAccount(generatePrivateKey()).address
  const transferHelperArtifact = async () => {
    return await hre.artifacts.readArtifact('NativeTransferHelper')
  }
  const callHelperArtifact = async () => {
    return await hre.artifacts.readArtifact('CallHelper')
  }

  it('CREATE + call value with overflowing deployer nonce does not emit an event', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const caller = randomAddress()

    // Deploy a NativeTransferHelper contract
    const transferHelper = await transferHelperArtifact()
    const callHelper = await callHelperArtifact()
    const nativeTransferHelperAddress = randomAddress()
    const stateOverride = [
      {
        address: nativeTransferHelperAddress,
        code: transferHelper.deployedBytecode,
        nonce: '0xffffffffffffffff', // uint64 max, so the contract cannot deploy new contracts
        balance: 10n,
      },
      {
        address: caller,
        balance: parseEther('1'),
      },
    ]

    const createCallData = encodeFunctionData({
      abi: transferHelper.abi,
      functionName: 'create',
      args: [callHelper.bytecode, 1n],
    })

    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [{ from: caller, to: nativeTransferHelperAddress, data: createCallData, value: 1n }],
          stateOverrides: stateOverride,
        },
      ],
    })

    const calls = blockSimulation[0].calls
    expect(calls).to.have.lengthOf(1)
    expect(calls[0].status).to.equal('success', 'Overall txn should succeed')
    expect(calls[0].logs).to.have.lengthOf(1, 'One native transfer event from the EOA to the contract')

    const events = parseEventLogs({
      abi: NativeCoinAuthority.abi,
      eventName: 'NativeCoinTransferred',
      logs: calls[0].logs,
    })
    expect(events).to.have.lengthOf(1)
    expect(events[0].args.from).to.equal(caller)
    expect(events[0].args.to).to.equal(nativeTransferHelperAddress)
    expect(events[0].args.amount).to.equal(1n)
  })

  it('CREATE + call value that overflows recipient balance does not emit an event', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const caller = randomAddress()

    // Deploy a NativeTransferHelper contract
    const helperArtifact = await transferHelperArtifact()
    const callHelper = await callHelperArtifact()
    const nativeTransferHelperAddress = '0x9c3Bc0295157658094dc1a9fF97eA6547a9B9320'
    // keccak(address, nonce = 1)
    const predictedDeployedAddress = '0xc5e3E4c43378a3e940275C854Ec8d210D2785A65'

    // Configure deployed-at address with uint256 max
    const stateOverride = [
      {
        address: nativeTransferHelperAddress,
        code: helperArtifact.deployedBytecode,
        balance: 0n,
        nonce: 1n,
      },
      {
        address: caller,
        balance: parseEther('1'),
      },
      {
        address: predictedDeployedAddress,
        balance: 2n ** 256n - 1n, // uint256 max
      },
    ]

    const createCallData = encodeFunctionData({
      abi: helperArtifact.abi,
      functionName: 'create',
      args: [callHelper.bytecode, 1n],
    })

    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [{ from: caller, to: nativeTransferHelperAddress, data: createCallData, value: 1n }],
          stateOverrides: stateOverride,
        },
      ],
    })

    const calls = blockSimulation[0].calls
    expect(calls).to.have.lengthOf(1)
    expect(calls[0].status).to.equal('success', 'Overall txn should succeed')
    expect(calls[0].logs).to.have.lengthOf(1, 'One native transfer event from the EOA to the contract')

    const events = parseEventLogs({
      abi: NativeCoinAuthority.abi,
      eventName: 'NativeCoinTransferred',
      logs: calls[0].logs,
    })
    expect(events).to.have.lengthOf(1)
    expect(events[0].args.from).to.equal(caller)
    expect(events[0].args.to).to.equal(nativeTransferHelperAddress)
    expect(events[0].args.amount).to.equal(1n)
  })

  it('CALL with value that overflows recipient balance does not emit an event', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const caller = randomAddress()
    const recipient = randomAddress()

    // Deploy a CallHelper contract
    const helperArtifact = await callHelperArtifact()
    const callHelperAddress = '0x9c3Bc0295157658094dc1a9fF97eA6547a9B9320'

    // Configure recipient with uint256 max balance
    const stateOverride = [
      {
        address: callHelperAddress,
        code: helperArtifact.deployedBytecode,
        balance: 0n,
        nonce: 1n,
      },
      {
        address: caller,
        balance: parseEther('1'),
      },
      {
        address: recipient,
        balance: 2n ** 256n - 1n, // uint256 max
      },
    ]

    const transferCallData = encodeFunctionData({
      abi: helperArtifact.abi,
      functionName: 'transfer',
      args: [recipient, 1n],
    })

    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [{ from: caller, to: callHelperAddress, data: transferCallData, value: 1n }],
          stateOverrides: stateOverride,
        },
      ],
    })

    const calls = blockSimulation[0].calls
    expect(calls).to.have.lengthOf(1)
    expect(calls[0].status).to.equal('success', 'Overall txn should succeed')

    const events = parseEventLogs({
      abi: NativeCoinAuthority.abi,
      eventName: 'NativeCoinTransferred',
      logs: calls[0].logs,
    })
    expect(events).to.have.lengthOf(1)
    expect(events[0].args.from).to.equal(caller)
    expect(events[0].args.to).to.equal(callHelperAddress)
    expect(events[0].args.amount).to.equal(1n)
  })
})
