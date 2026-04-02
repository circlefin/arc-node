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
import { encodeFunctionData, zeroAddress } from 'viem'
import { SystemAccounting } from '../helpers/SystemAccounting'

describe('SystemAccounting', () => {
  it('simulate storeGasvalues', async () => {
    const { client } = await getClients()

    const res = await client.simulateCalls({
      account: zeroAddress,
      calls: [
        {
          to: SystemAccounting.address,
          data: encodeFunctionData({
            abi: SystemAccounting.abi,
            functionName: 'storeGasValues',
            args: [123456n, { gasUsed: 100000n, gasUsedSmoothed: 95000n, nextBaseFee: 300n }],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // Zero5: EIP-2929 warm/cold gas pricing for SSTORE
    expectGasUSedApproximately(res.results[0].gasUsed, 30908n)
  })

  it('delegatecall fails with authorization error', async () => {
    const { client, sender } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const storeGasValuesCalldata = encodeFunctionData({
      abi: SystemAccounting.abi,
      functionName: 'storeGasValues',
      args: [123456n, { gasUsed: 100000n, gasUsedSmoothed: 95000n, nextBaseFee: 300n }],
    })

    const res = await client.simulateCalls({
      account: sender.account.address, // Not authorized
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [SystemAccounting.address, storeGasValuesCalldata],
          }),
        },
      ],
    })

    expect(res.results[0].status).to.be.eq('success')
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Invalid caller'),
    })
  })

  it('delegatecall fails even if authorized', async () => {
    const { client, sender } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const storeGasValuesCalldata = encodeFunctionData({
      abi: SystemAccounting.abi,
      functionName: 'storeGasValues',
      args: [123456n, { gasUsed: 100000n, gasUsedSmoothed: 95000n, nextBaseFee: 300n }],
    })

    const res = await client.simulateCalls({
      account: zeroAddress, // Authorized
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [SystemAccounting.address, storeGasValuesCalldata],
          }),
        },
      ],
    })

    expect(res.results[0].status).to.be.eq('success')
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Delegate call not allowed'),
    })
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
              { allowFailure: true, target: SystemAccounting.address, data: '0xc0ffee' },
              { allowFailure: true, target: SystemAccounting.address, data: '0xdeadbeef' },
              { allowFailure: true, target: SystemAccounting.address, data: '0x9350ff5c' }, // storeGasValues selector
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
})
