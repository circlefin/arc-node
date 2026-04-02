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
import {
  Account,
  Address,
  Chain,
  Client,
  decodeFunctionData,
  encodeErrorResult,
  encodeFunctionData,
  encodeFunctionResult,
  getContract,
  Hex,
  parseAbi,
  Transport,
} from 'viem'
import { PublicClient, WalletClient } from '@nomicfoundation/hardhat-viem/types'
import { KeyedClient } from './client-extension'
import { DeterministicDeployerProxy } from './DeterministicDeployerProxy'

export type Call3Value = {
  target: Address
  allowFailure?: boolean
  value?: bigint
  data?: Hex | CallInput
}

export const multicall3Abi = parseAbi([
  'function aggregate3Value(Call3Value[] calldata calls) external payable returns (Result[] memory returnData)',
  'struct Call3Value { address target; bool allowFailure; uint256 value; bytes callData; }',
  'struct Result { bool success; bytes returnData; }',
])

export type CallInput =
  | {
      fn: 'execute' | 'staticCall' | 'delegateCall' | 'callCode'
      target: Address
      data?: Hex | CallInput
      value?: bigint
    }
  | { fn: 'transfer'; to: Address; value: bigint }
  | { fn: 'revertWithString' | 'revertWithError'; message: string }
  | { fn: 'executeBatch'; calls: Array<Call3Value> }
  | { fn: 'setStorage'; slot: bigint; value: bigint }
  | { fn: 'getStorage'; slot: bigint }
  | { fn: 'getBlockInfo' | 'getTxInfo' }

export type CallResult =
  | { success: boolean; result: Hex }
  | { success: false; revertString: string }
  | { success: false; revertError: string }
  | { success: boolean; nested: CallResult }
  | { fn: 'getStorage'; value: bigint }
  | { fn: 'executeBatch'; results: Array<CallResult> }

export const callHelperArtifact = hre.artifacts.readArtifactSync('CallHelper')

export class CallHelper {
  static abi = callHelperArtifact.abi

  static deploy = async (wallet: WalletClient, client: PublicClient, value: bigint = 0n) => {
    const receipt = await wallet
      .deployContract({
        abi: callHelperArtifact.abi,
        bytecode: callHelperArtifact.bytecode,
        args: [],
        value,
      })
      .then((hash) => client.waitForTransactionReceipt({ hash }))
    if (receipt.contractAddress == null) {
      throw new Error('Deployment failed, missing contract address')
    }
    return CallHelper.attach({ wallet, public: client }, receipt.contractAddress)
  }

  static deterministicDeploy = async (
    wallet: WalletClient,
    client: PublicClient,
    balanceLowerBound = 0n,
    salt = 0n,
  ) => {
    const address = await DeterministicDeployerProxy.deployCode(wallet, client, callHelperArtifact.bytecode, salt)
    const balc = await client.getBalance({ address })
    if (balanceLowerBound > 0n && balc < balanceLowerBound) {
      await wallet.sendTransaction({ to: address, value: balanceLowerBound * 2n })
    }
    return CallHelper.attach({ wallet, public: client }, address)
  }

  static attach = <
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    const CC extends Client<T, C, A> | KeyedClient<T, C, A>,
  >(
    client: CC,
    address: Address,
  ) => {
    return getContract({ abi: CallHelper.abi, address, client })
  }

  static encodeError = (msg: string): Hex =>
    encodeErrorResult({ abi: callHelperArtifact.abi, errorName: 'ErrorMessage', args: [msg] })

  static encodeRevertMessage = (msg: string): Hex =>
    encodeErrorResult({ abi: parseAbi(['error Error(string)']), errorName: 'Error', args: [msg] })

  /**
   * decode the nested call data for debugging.
   */
  static decodeNested = (data: Hex): Hex | CallInput => {
    if (data === '0x') {
      return '0x'
    }
    const { functionName: fn, args } = decodeFunctionData({ abi: CallHelper.abi, data })
    switch (fn) {
      case 'execute':
      case 'callCode':
        return { fn, target: args[0], data: this.decodeNested(args[1]), value: args[2] }
      case 'staticCall':
      case 'delegateCall':
        return { fn, target: args[0], data: this.decodeNested(args[1]) }
      case 'transfer':
        return { fn, to: args[0], value: args[1] }
      case 'revertWithString':
      case 'revertWithError':
        return { fn, message: args[0] }
      case 'setStorage':
        return { fn, slot: args[0], value: args[1] }
      case 'getStorage':
        return { fn, slot: args[0] }
      case 'getBlockInfo':
      case 'getTxInfo':
        return { fn }
      case 'executeBatch':
        return {
          fn,
          calls: args[0].map((call) => ({
            target: call.target,
            allowFailure: call.allowFailure,
            value: call.value,
            data: this.decodeNested(call.callData),
          })),
        }
    }
    return data
  }

  static encodeNested = (input: CallInput | Hex): Hex => {
    if (typeof input === 'string') {
      return input
    }
    const functionName = input.fn
    const abi = callHelperArtifact.abi
    switch (functionName) {
      case 'execute':
      case 'callCode':
        return encodeFunctionData({
          abi,
          functionName,
          args: [input.target, this.encodeNested(input.data ?? '0x'), input.value ?? 0n],
        })
      case 'staticCall':
      case 'delegateCall':
        return encodeFunctionData({ abi, functionName, args: [input.target, this.encodeNested(input.data ?? '0x')] })
      case 'transfer':
        return encodeFunctionData({ abi, functionName, args: [input.to, input.value] })
      case 'revertWithString':
      case 'revertWithError':
        return encodeFunctionData({ abi, functionName, args: [input.message] })
      case 'setStorage':
        return encodeFunctionData({ abi, functionName, args: [input.slot, input.value] })
      case 'getStorage':
        return encodeFunctionData({ abi, functionName, args: [input.slot] })
      case 'getBlockInfo':
      case 'getTxInfo':
        return encodeFunctionData({ abi, functionName, args: [] })
      case 'executeBatch':
        return encodeFunctionData({
          abi,
          functionName,
          args: [
            input.calls.map((call) => ({
              allowFailure: call.allowFailure ?? false,
              target: call.target,
              value: call.value ?? 0n,
              callData: this.encodeNested(call.data ?? '0x'),
            })),
          ],
        })
    }
    // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
    throw new Error(`Unknown function name: ${input}`)
  }

  static result = (result: CallResult): { success: boolean; result: Hex } => {
    if (!('nested' in result) && !('fn' in result)) {
      if (result.success || 'result' in result) {
        return result
      }
      return {
        success: false,
        result:
          'revertString' in result
            ? CallHelper.encodeRevertMessage(result.revertString)
            : CallHelper.encodeError(result.revertError),
      }
    }

    if ('fn' in result) {
      switch (result.fn) {
        case 'getStorage':
          return {
            success: true,
            result: encodeFunctionResult({ abi: CallHelper.abi, functionName: result.fn, result: result.value }),
          }
        case 'executeBatch':
          return {
            success: true,
            result: encodeFunctionResult({
              abi: CallHelper.abi,
              functionName: result.fn,
              result: result.results.map((res) => {
                const data = CallHelper.result(res)
                return { success: data.success, returnData: data.result }
              }),
            }),
          }
      }
      throw new Error(`Unknown function name: ${(result as unknown as { fn: string }).fn}`)
    }

    const nested = CallHelper.result(result.nested)
    return {
      success: result.success,
      result: encodeFunctionResult({
        abi: CallHelper.abi,
        functionName: 'execute',
        result: [nested.success, nested.result],
      }),
    }
  }
}
