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

// Pre-EIP-155 keyless deployment-style fixture: legacy envelope with v=27,
// r=s=1, and no chain_id in the signature.
const KEYLESS_RAW_TX = '0xe380843b9aca0082520894000000000000000000000000000000000000000080801b0101'

const EXPECTED_ERROR_CODE = -32000
const EXPECTED_ERROR_MESSAGE = 'only replay-protected (EIP-155) transactions allowed over RPC'

type JsonRpcParams = unknown[] | Record<string, unknown>

interface JsonRpcResponse {
  jsonrpc: '2.0'
  id: number
  result?: unknown
  error?: { code: number; message: string; data?: unknown }
}

interface JsonRpcRequest {
  jsonrpc: '2.0'
  id: number
  method: string
  params: JsonRpcParams
}

function rpcUrl(): string {
  const rpcUrl = hre.network.config.url as string
  if (!rpcUrl) {
    throw new Error('hre.network.config.url is required for this test')
  }
  return rpcUrl
}

async function postRpc(body: JsonRpcRequest | JsonRpcRequest[]): Promise<unknown> {
  const response = await fetch(rpcUrl(), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!response.ok) {
    throw new Error(`HTTP ${response.status} ${response.statusText}`)
  }
  return response.json()
}

async function rpcCall(method: string, params: JsonRpcParams): Promise<JsonRpcResponse> {
  return (await postRpc({ jsonrpc: '2.0', id: 1, method, params })) as JsonRpcResponse
}

async function rpcBatchCall(requests: JsonRpcRequest[]): Promise<JsonRpcResponse[]> {
  return (await postRpc(requests)) as JsonRpcResponse[]
}

describe('Pre-EIP-155 raw transactions', () => {
  // Scenario: A keyless pre-EIP-155 raw transaction is submitted through the public RPC.
  // Call flow: test -> JSON-RPC -> eth_sendRawTransaction
  // Assertions: the RPC rejects it with the Arc replay-protection error.
  it('rejects keyless legacy transactions over RPC', async () => {
    const res = await rpcCall('eth_sendRawTransaction', [KEYLESS_RAW_TX])

    expect(res.result, 'expected no result when error is returned').to.be.undefined
    expect(res.error, 'expected an error response').to.exist
    expect(res.error!.code).to.equal(EXPECTED_ERROR_CODE)
    expect(res.error!.message).to.equal(EXPECTED_ERROR_MESSAGE)
  })

  // Scenario: A keyless pre-EIP-155 raw transaction is submitted with object-form params.
  // Call flow: test -> JSON-RPC -> eth_sendRawTransaction({ bytes })
  // Assertions: the RPC rejects it with the Arc replay-protection error.
  it('rejects keyless legacy transactions over RPC with object params', async () => {
    const res = await rpcCall('eth_sendRawTransaction', { bytes: KEYLESS_RAW_TX })

    expect(res.result, 'expected no result when error is returned').to.be.undefined
    expect(res.error, 'expected an error response').to.exist
    expect(res.error!.code).to.equal(EXPECTED_ERROR_CODE)
    expect(res.error!.message).to.equal(EXPECTED_ERROR_MESSAGE)
  })

  // Scenario: A JSON-RPC batch mixes a harmless request with a keyless pre-EIP-155 raw transaction.
  // Call flow: test -> JSON-RPC batch -> eth_chainId + eth_sendRawTransaction
  // Assertions: the valid entry succeeds while only the keyless transaction entry is rejected.
  it('rejects only the keyless transaction entry in a JSON-RPC batch', async () => {
    const batch = await rpcBatchCall([
      { jsonrpc: '2.0', id: 1, method: 'eth_chainId', params: [] },
      { jsonrpc: '2.0', id: 2, method: 'eth_sendRawTransaction', params: [KEYLESS_RAW_TX] },
    ])

    const responses = new Map(batch.map((res) => [res.id, res]))
    const chainId = responses.get(1)
    const sendRawTransaction = responses.get(2)

    expect(chainId, 'expected eth_chainId response').to.exist
    expect(chainId!.error, 'expected no eth_chainId error').to.be.undefined
    expect(chainId!.result, 'expected eth_chainId result').to.be.a('string')

    expect(sendRawTransaction, 'expected eth_sendRawTransaction response').to.exist
    expect(sendRawTransaction!.result, 'expected no result when error is returned').to.be.undefined
    expect(sendRawTransaction!.error, 'expected an error response').to.exist
    expect(sendRawTransaction!.error!.code).to.equal(EXPECTED_ERROR_CODE)
    expect(sendRawTransaction!.error!.message).to.equal(EXPECTED_ERROR_MESSAGE)
  })
})
