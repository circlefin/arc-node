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
import { MethodNotFoundRpcError, toHex } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { getClients } from '../helpers/networks'

const BUNDLE_RPC_METHODS = [
  'eth_callBundle',
  'eth_sendBundle',
  'eth_cancelBundle',
  'eth_sendPrivateTransaction',
  'eth_sendPrivateRawTransaction',
  'eth_cancelPrivateTransaction',
] as const

type CallBundleParams = [
  {
    txs: [`0x${string}`]
    blockNumber: `0x${string}`
    stateBlockNumber: 'latest'
    baseFee: '0x0'
  },
]

describe('Bundle RPC methods', () => {
  for (const method of BUNDLE_RPC_METHODS) {
    // Scenario: A bundle or private-transaction method is called through the public HTTP RPC.
    // Call flow: test → HTTP JSON-RPC → public RPC method lookup
    // Assertions: the method is absent and returns the standard method-not-found error.
    it(`does not expose ${method}`, async () => {
      const { client } = await getClients()
      await expect(client.request<{ Parameters: []; ReturnType: unknown }>({ method, params: [] })).to.be.rejectedWith(
        MethodNotFoundRpcError,
      )
    })
  }

  // Scenario: A valid executable bundle is submitted through the public HTTP RPC.
  // Call flow: fresh signer → signed zero-fee transfer → eth_callBundle simulation
  // Assertions: eth_callBundle is absent even when the request would execute successfully in unmodified Reth.
  it('does not execute eth_callBundle with valid params', async () => {
    const { chain, client } = await getClients()
    const account = privateKeyToAccount(generatePrivateKey())
    const rawTransaction = await account.signTransaction({
      chainId: chain.id,
      gas: 21_000n,
      gasPrice: 0n,
      nonce: 0,
      to: account.address,
      value: 0n,
    })
    const params: CallBundleParams = [
      {
        txs: [rawTransaction],
        blockNumber: toHex((await client.getBlockNumber()) + 1n),
        stateBlockNumber: 'latest',
        baseFee: '0x0',
      },
    ]

    await expect(
      client.request<{ Parameters: CallBundleParams; ReturnType: unknown }>({ method: 'eth_callBundle', params }),
    ).to.be.rejectedWith(MethodNotFoundRpcError)
  })
})
