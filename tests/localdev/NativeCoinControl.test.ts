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
import { EventsVerifier, expectGasUSedApproximately, getClients } from '../helpers'
import { CallHelper } from '../helpers/CallHelper'
import { encodeFunctionData } from 'viem'
import { NativeCoinControl } from '../helpers/NativeCoinControl'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { USDC } from '../helpers/FiatToken'

describe('NativeCoinControl', () => {
  it('simulate blocklist', async () => {
    const { client } = await getClients()
    const randAccount = privateKeyToAccount(generatePrivateKey())

    const res = await client.simulateCalls({
      account: USDC.address,
      calls: [
        {
          to: NativeCoinControl.address,
          data: encodeFunctionData({
            abi: NativeCoinControl.abi,
            functionName: 'blocklist',
            args: [randAccount.address],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // Zero5: cold SSTORE 0→non-zero = 22100 (EIP-2200), event = 1125
    // Precompile internal cost (no base tx cost in simulation)
    expectGasUSedApproximately(res.results[0].gasUsed, 44657n)
  })

  it('simulate isBlocklisted', async () => {
    const { client, sender } = await getClients()

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        {
          to: NativeCoinControl.address,
          data: encodeFunctionData({
            abi: NativeCoinControl.abi,
            functionName: 'isBlocklisted',
            args: [sender.account.address],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // Zero5: cold SLOAD = 2100 for blocklist check
    expect(res.results[0].gasUsed).to.be.greaterThanOrEqual(21000n + 100n)
    expectGasUSedApproximately(res.results[0].gasUsed, 24180n)
  })

  it('invalid selector', async () => {
    const { client, sender } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        {
          to: helper.address,
          data: CallHelper.encodeNested({
            fn: 'executeBatch',
            calls: [
              { allowFailure: true, target: NativeCoinControl.address, data: '0xc0ffee' },
              { allowFailure: true, target: NativeCoinControl.address, data: '0xdeadbeef' },
              { allowFailure: true, target: NativeCoinControl.address, data: '0xe5c7160b' },
            ],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    EventsVerifier.fromSimulationLogs(res.results[0].logs)
      .expectExecutionResult({ helper, success: false, result: CallHelper.encodeRevertMessage('Input too short') })
      .expectExecutionResult({ helper, success: false, result: CallHelper.encodeRevertMessage('Invalid selector') })
      .expectExecutionResult({ helper, success: false, result: CallHelper.encodeRevertMessage('Execution reverted') })
  })

  it('delegatecall blocklist() and unBlocklist() fails authorization check', async () => {
    const { client, sender, receiver } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const blocklistCalldata = encodeFunctionData({
      abi: NativeCoinControl.abi,
      functionName: 'blocklist',
      args: [receiver.account.address],
    })

    const unBlocklistCalldata = encodeFunctionData({
      abi: NativeCoinControl.abi,
      functionName: 'unBlocklist',
      args: [receiver.account.address],
    })

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinControl.address, blocklistCalldata],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinControl.address, unBlocklistCalldata],
          }),
        },
      ],
    })

    // Always succeeds
    expect(res.results[0].status).to.be.eq('success')
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled for blocklisting'),
    })
    EventsVerifier.fromSimulationLogs(res.results[1].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled for unblocklisting'),
    })
  })

  it('delegatecall blocklist() and unBlocklist() fails delegatecall check if authorized', async () => {
    const { client, sender, receiver } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const blocklistCalldata = encodeFunctionData({
      abi: NativeCoinControl.abi,
      functionName: 'blocklist',
      args: [receiver.account.address],
    })

    const unBlocklistCalldata = encodeFunctionData({
      abi: NativeCoinControl.abi,
      functionName: 'unBlocklist',
      args: [receiver.account.address],
    })

    const res = await client.simulateCalls({
      account: USDC.address,
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinControl.address, blocklistCalldata],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinControl.address, unBlocklistCalldata],
          }),
        },
      ],
    })

    // Always succeeds
    expect(res.results[0].status).to.be.eq('success')
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Delegate call not allowed'),
    })
    EventsVerifier.fromSimulationLogs(res.results[1].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Delegate call not allowed'),
    })
  })
})
