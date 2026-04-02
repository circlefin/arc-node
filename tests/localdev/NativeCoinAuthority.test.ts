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
import {
  EventsVerifier,
  expectGasUSedApproximately,
  getClients,
  NativeCoinAuthority,
  ReceiptVerifier,
} from '../helpers'
import { CallHelper } from '../helpers/CallHelper'
import { encodeFunctionData, EstimateGasExecutionError, parseEther } from 'viem'
import { FIAT_TOKEN_ADDRESS } from '../../scripts/genesis/NativeFiatToken'
import { USDC } from '../helpers/FiatToken'

describe('NativeCoinAuthority', () => {
  const errNotMinter = CallHelper.encodeRevertMessage('Not enabled native coin minter')

  // Zero5+ gas costs with EIP-2929 warm/cold awareness
  // Precompile costs (excludes ~21000 base tx + ~2500 call overhead):
  //   mint/burn:   cold=13481, warm=6681  (blocklist + total_supply r/w + balance_incr/decr + event)
  //   transfer:    cold=15956, warm=11956 (2 blocklist + transfer + event)
  //   transfer(0): cold=4200,  warm=200   (2 blocklist checks only)
  //   invalid:     0 gas (immediate revert on auth check)

  it('simulate mint', async () => {
    const { client, sender } = await getClients()

    const res = await client.simulateCalls({
      account: FIAT_TOKEN_ADDRESS,
      calls: [
        {
          to: NativeCoinAuthority.address,
          data: encodeFunctionData({
            abi: NativeCoinAuthority.abi,
            functionName: 'mint',
            args: [sender.account.address, 1n],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // mint: min=21000+6681=27681 (warm), max=21000+13481=34481 (cold), +overhead
    expect(res.results[0].gasUsed).to.be.greaterThanOrEqual(21000n + 6681n)
    expectGasUSedApproximately(res.results[0].gasUsed, 37153n)
    expect(res.results[0].logs).to.have.lengthOf(1)
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectNativeMint({
      recipient: sender.account.address,
      amount: 1n,
    })
  })

  it('simulate burn', async () => {
    const { client, sender } = await getClients()

    const res = await client.simulateCalls({
      account: FIAT_TOKEN_ADDRESS,
      calls: [
        {
          to: NativeCoinAuthority.address,
          data: encodeFunctionData({
            abi: NativeCoinAuthority.abi,
            functionName: 'burn',
            args: [sender.account.address, 1n],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // burn: min=21000+6681=27681 (warm), max=21000+13481=34481 (cold), +overhead
    expect(res.results[0].gasUsed).to.be.greaterThanOrEqual(21000n + 6681n)
    expectGasUSedApproximately(res.results[0].gasUsed, 37153n)
    expect(res.results[0].logs).to.have.lengthOf(1)
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectNativeBurn({
      from: sender.account.address,
      amount: 1n,
    })
  })

  it('simulate transfer with non-zero amount', async () => {
    const { client, sender, receiver } = await getClients()

    const res = await client.simulateCalls({
      account: FIAT_TOKEN_ADDRESS,
      calls: [
        {
          to: NativeCoinAuthority.address,
          data: encodeFunctionData({
            abi: NativeCoinAuthority.abi,
            functionName: 'transfer',
            args: [sender.account.address, receiver.account.address, 1n],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // transfer: min=21000+11956=32956 (warm), max=21000+15956=36956 (cold), +overhead
    expect(res.results[0].gasUsed).to.be.greaterThanOrEqual(21000n + 11956n)
    expectGasUSedApproximately(res.results[0].gasUsed, 39996n)
    expect(res.results[0].logs).to.have.lengthOf(1)
    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectNativeTransfer({
      from: sender.account.address,
      to: receiver.account.address,
      amount: 1n,
    })
  })

  it('simulate transfer with zero amount', async () => {
    const { client, sender, receiver } = await getClients()

    const res = await client.simulateCalls({
      account: FIAT_TOKEN_ADDRESS,
      calls: [
        {
          to: NativeCoinAuthority.address,
          data: encodeFunctionData({
            abi: NativeCoinAuthority.abi,
            functionName: 'transfer',
            args: [sender.account.address, receiver.account.address, 0n],
          }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
    // transfer(0): min=21000+200=21200 (warm), max=21000+4200=25200 (cold), +overhead
    expect(res.results[0].gasUsed).to.be.greaterThanOrEqual(21000n + 200n)
    expectGasUSedApproximately(res.results[0].gasUsed, 28228n)
    expect(res.results[0].logs).to.have.lengthOf(0)
  })

  it('invalid minter', async () => {
    const { client, sender } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const mintData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'mint',
      args: [sender.account.address, 1n],
    })

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        { to: NativeCoinAuthority.address, data: mintData },
        {
          to: helper.address,
          data: CallHelper.encodeNested({ fn: 'execute', target: NativeCoinAuthority.address, data: mintData }),
        },
      ],
    })

    expect(res.results[0].status).to.be.eq('failure')
    expect(res.results[0].data).to.be.eq(errNotMinter)
    // invalid minter direct call: immediate revert, ~21000 base + overhead
    expectGasUSedApproximately(res.results[0].gasUsed, 24530n)

    expect(res.results[1].status).to.be.eq('success')
    // invalid minter via helper: helper call overhead + inner revert
    expectGasUSedApproximately(res.results[1].gasUsed, 31112n)

    EventsVerifier.fromSimulationLogs(res.results[1].logs).expectExecutionResult({
      helper,
      success: false,
      result: errNotMinter,
    })

    await expect(
      client.estimateGas({
        account: sender.account.address,
        to: NativeCoinAuthority.address,
        data: mintData,
      }),
    ).to.rejectedWith(EstimateGasExecutionError, 'Execution reverted with reason: Not enabled native coin minter.')

    const est = await client.estimateGas({
      account: sender.account.address,
      to: helper.address,
      data: CallHelper.encodeNested({ fn: 'execute', target: NativeCoinAuthority.address, data: mintData }),
    })
    // estimateGas includes buffer for gas estimation
    expectGasUSedApproximately(est, 31464n)

    const receipt = await sender
      .sendTransaction({
        to: helper.address,
        data: CallHelper.encodeNested({ fn: 'execute', target: NativeCoinAuthority.address, data: mintData }),
      })
      .then(ReceiptVerifier.waitSuccess)
    // invalid minter via helper: helper call overhead + inner revert
    receipt.verifyGasUsedApproximately(31112n)
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
              { allowFailure: true, target: NativeCoinAuthority.address, data: '0xc0ffee' },
              { allowFailure: true, target: NativeCoinAuthority.address, data: '0xdeadbeef' },
              { allowFailure: true, target: NativeCoinAuthority.address, data: '0x40c10f19' },
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

  it('delegatecalls to protected functions fail with authorization error', async () => {
    const { client, sender, receiver } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const mintCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'mint',
      args: [sender.account.address, parseEther('1')],
    })

    const burnCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'burn',
      args: [sender.account.address, parseEther('1')],
    })

    const transferCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'transfer',
      args: [sender.account.address, receiver.account.address, parseEther('1')],
    })

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, mintCallData],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, burnCallData],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, transferCallData],
          }),
        },
      ],
    })

    // CallHelper.sol always returns success here, regardless of the delegatecall result
    expect(res.results[0].status).to.be.eq('success')
    expect(res.results[1].status).to.be.eq('success')
    expect(res.results[2].status).to.be.eq('success')

    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled native coin minter'),
    })
    EventsVerifier.fromSimulationLogs(res.results[1].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled native coin burner'),
    })
    EventsVerifier.fromSimulationLogs(res.results[2].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled for native coin transfers'),
    })
  })

  it('even if authorized, delegatecalls to protected functions fail', async () => {
    const { client, sender, receiver } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const mintCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'mint',
      args: [sender.account.address, parseEther('1')],
    })

    const burnCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'burn',
      args: [sender.account.address, parseEther('1')],
    })

    const transferCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'transfer',
      args: [sender.account.address, receiver.account.address, parseEther('1')],
    })

    const res = await client.simulateCalls({
      account: USDC.address,
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, mintCallData],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, burnCallData],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'delegateCall',
            args: [NativeCoinAuthority.address, transferCallData],
          }),
        },
      ],
    })

    // CallHelper.sol always returns success here, regardless of the delegatecall result
    expect(res.results[0].status).to.be.eq('success')
    expect(res.results[1].status).to.be.eq('success')
    expect(res.results[2].status).to.be.eq('success')

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
    EventsVerifier.fromSimulationLogs(res.results[2].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Delegate call not allowed'),
    })
  })

  it('callCode to protected functions fails with authorization error', async () => {
    const { client, sender, receiver } = await getClients()
    const helper = await CallHelper.deterministicDeploy(sender, client)

    const mintCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'mint',
      args: [sender.account.address, parseEther('1')],
    })

    const burnCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'burn',
      args: [sender.account.address, parseEther('1')],
    })

    const transferCallData = encodeFunctionData({
      abi: NativeCoinAuthority.abi,
      functionName: 'transfer',
      args: [sender.account.address, receiver.account.address, parseEther('1')],
    })

    const res = await client.simulateCalls({
      account: sender.account.address,
      calls: [
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'callCode',
            args: [NativeCoinAuthority.address, mintCallData, 0n],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'callCode',
            args: [NativeCoinAuthority.address, burnCallData, 0n],
          }),
        },
        {
          to: helper.address,
          data: encodeFunctionData({
            abi: CallHelper.abi,
            functionName: 'callCode',
            args: [NativeCoinAuthority.address, transferCallData, 0n],
          }),
        },
      ],
    })

    // CallHelper.sol always returns success here, regardless of the delegatecall result
    expect(res.results[0].status).to.be.eq('success')
    expect(res.results[1].status).to.be.eq('success')
    expect(res.results[2].status).to.be.eq('success')

    EventsVerifier.fromSimulationLogs(res.results[0].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled native coin minter'),
    })
    EventsVerifier.fromSimulationLogs(res.results[1].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled native coin burner'),
    })
    EventsVerifier.fromSimulationLogs(res.results[2].logs).expectExecutionResult({
      helper,
      success: false,
      result: CallHelper.encodeRevertMessage('Not enabled for native coin transfers'),
    })
  })
})
