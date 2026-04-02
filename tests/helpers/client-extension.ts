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

import { Hex, Address, Hash, Transport, Chain, Account, RpcSchema, Client, BlockTag, BlockNumber, toHex } from 'viem'
import { Bytes32 } from '../../scripts/genesis/types'

export type TracerOption =
  | {
      tracer: 'callTracer'
      tracerConfig?: {
        onlyTopCall?: boolean
        withLog?: boolean
      }
    }
  | {
      tracer: 'prestateTracer'
      tracerConfig?: {
        diffMode?: boolean
        disableCode?: boolean
        disableStorage?: boolean
      }
    }
  | {
      tracer: string // JavaScript tracer code
      timeout?: string
    }

export type TraceResponse<
  opt extends TracerOption,
  Response = opt['tracer'] extends 'callTracer'
    ? CallFrame
    : opt['tracer'] extends 'prestateTracer'
      ? opt['tracerConfig'] extends { diffMode: true }
        ? { pre: AccountStates; post: AccountStates }
        : AccountStates
      : unknown, // JavaScript tracer can return any structure
> = Response

export type AccountStates<Quantity = Hex> = {
  [address: Address]: {
    balance: Quantity
    nonce: Quantity
    code: Hex
    storage: Record<Bytes32, Bytes32>
  }
}

export type FrameLog = {
  address: Address
  topics?: [Bytes32, ...Bytes32[]] | []
  data?: Hex
  position: Hex
}

export type CallInput<Quantity = bigint> = {
  from?: Address
  to: Address
  value?: Quantity
  data?: Hex
  gas?: Quantity
  gasPrice?: Quantity
}

export type CallType = 'CALL' | 'DELEGATECALL' | 'STATICCALL' | 'CREATE' | 'CREATE2' | 'CALLCODE'

export type CallFrame<Quantity = Hex> = Omit<CallInput<Quantity>, 'data' | 'gasPrice'> & {
  gasUsed?: Quantity
  input?: Hex
  output?: Hex
  type?: CallType
  logs?: Array<FrameLog>
  calls?: CallFrame[]
  error?: string
  revertReason?: string
}

export type BlockCallFrames = Array<{ result: CallFrame; txHash: Hash }>

export type TraceBlockResponse<TTracerOption extends TracerOption, TxResponse = TraceResponse<TTracerOption>> = Array<{
  result: TxResponse
  txHash: Hash
}>

export const debugTraceFunctions = <
  T extends Transport,
  C extends Chain | undefined,
  A extends Account | undefined,
  R extends RpcSchema | undefined,
>(
  c: Client<T, C, A, R>,
) => ({
  traceTransaction: async <TTracerOption extends TracerOption, R = TraceResponse<TTracerOption>>(
    txHash: Hash,
    opts: TTracerOption,
  ) =>
    await c.request<{
      Method: 'debug_traceTransaction'
      Parameters: [Hash, TracerOption]
      ReturnType: R
    }>({
      method: 'debug_traceTransaction',
      params: [txHash, opts],
    }),

  traceCall: async <TTracerOption extends TracerOption, Response = TraceResponse<TTracerOption>>(
    callParam: CallInput,
    blockTag: BlockNumber<bigint> | BlockTag,
    opts: TTracerOption,
  ) =>
    await c.request<{
      Method: 'debug_traceCall'
      Parameters: [CallInput<Hex>, BlockNumber<Hex> | BlockTag, TTracerOption]
      ReturnType: Response
    }>({
      method: 'debug_traceCall',
      params: [
        {
          from: callParam.from,
          to: callParam.to,
          value: callParam.value ? toHex(callParam.value) : undefined,
          data: callParam.data,
          gas: callParam.gas ? toHex(callParam.gas) : undefined,
          gasPrice: callParam.gasPrice ? toHex(callParam.gasPrice) : undefined,
        },
        typeof blockTag === 'bigint' ? toHex(blockTag) : blockTag,
        opts,
      ],
    }),

  traceBlockByHash: async <TTracerOption extends TracerOption, Response = TraceBlockResponse<TTracerOption>>(
    blockHash: Bytes32,
    opts: TTracerOption,
  ) =>
    await c.request<{
      Method: 'debug_traceBlockByHash'
      Parameters: [Bytes32, TTracerOption]
      ReturnType: Response
    }>({
      method: 'debug_traceBlockByHash',
      params: [blockHash, opts],
    }),

  traceBlockByNumber: async <TTracerOption extends TracerOption, Response = TraceBlockResponse<TTracerOption>>(
    blockTag: BlockNumber<bigint> | BlockTag,
    opts: TTracerOption,
  ) =>
    await c.request<{
      Method: 'debug_traceBlockByNumber'
      Parameters: [BlockNumber<Hex> | BlockTag, TTracerOption]
      ReturnType: Response
    }>({
      method: 'debug_traceBlockByNumber',
      params: [typeof blockTag === 'bigint' ? toHex(blockTag) : blockTag, opts],
    }),
})

// Copy the private type define from viem to enable the full ability of getContract.
// https://github.com/wevm/viem/blob/viem%402.33.1/src/actions/getContract.ts
export type KeyedClient<
  transport extends Transport = Transport,
  chain extends Chain | undefined = Chain | undefined,
  account extends Account | undefined = Account | undefined,
> =
  | {
      public?: Client<transport, chain> | undefined
      wallet: Client<transport, chain, account>
    }
  | {
      public: Client<transport, chain>
      wallet?: Client<transport, chain, account> | undefined
    }
