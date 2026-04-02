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

import { Address, parseAbi, PublicClient, RpcSchema, Transport, Chain, Account, ReadContractReturnType } from 'viem'
import { systemAccountingAddress } from '../../scripts/genesis'
import { isArcNetwork } from './networks'
import { clampBaseFee, ProtocolConfig } from './ProtocolConfig'

type GetGasValuesReturn = ReadContractReturnType<typeof SystemAccounting.abi, 'getGasValues'>

export class SystemAccounting {
  static readonly address: Address = systemAccountingAddress

  static readonly abi = parseAbi([
    'struct GasValues { uint64 gasUsed; uint64 gasUsedSmoothed; uint64 nextBaseFee; }',
    'function getGasValues(uint64 blockNumber) external view returns (GasValues gasValues)',
    'function storeGasValues(uint64 blockNumber, GasValues gasValues) external returns (bool success)',
  ])

  static getGasValues = async <
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    R extends RpcSchema | undefined,
  >(
    client: PublicClient<T, C, A, R>,
    blockNumber: bigint,
  ): Promise<GetGasValuesReturn> => {
    return client.readContract({
      address: SystemAccounting.address,
      abi: SystemAccounting.abi,
      functionName: 'getGasValues',
      args: [blockNumber],
      blockNumber,
    })
  }
}

export const getNextBaseFee = async (client: PublicClient, blockNumber?: bigint) => {
  const block = await client.getBlock({ blockNumber, includeTransactions: false })
  const isArc = isArcNetwork()

  const feeParams = await (async () => {
    try {
      return await ProtocolConfig.attach(client).read.feeParams({ blockNumber: block.number })
    } catch (_err) {
      return undefined
    }
  })()

  const nextEIP1559BaseFee = calc1559BaseFee(block.gasUsed, block.gasLimit, block.baseFeePerGas ?? 0n, feeParams)
  const { gasUsed, gasUsedSmoothed, nextBaseFee } = isArc
    ? await SystemAccounting.getGasValues(client, block.number)
    : { gasUsed: 0n, gasUsedSmoothed: 0n, nextBaseFee: 0n }
  const nextEMABaseFee = isArc
    ? feeParams
      ? clampBaseFee(calc1559BaseFee(gasUsedSmoothed, block.gasLimit, block.baseFeePerGas ?? 0n, feeParams), feeParams)
      : nextEIP1559BaseFee
    : nextEIP1559BaseFee

  return {
    block,
    gasValues: { gasUsed, gasUsedSmoothed, nextBaseFee },
    feeParams,
    next: {
      baseFeePerGas: isArc ? nextEMABaseFee : nextEIP1559BaseFee,
      emaBaseFee: nextEMABaseFee,
      eip1559BaseFee: nextEIP1559BaseFee,
    },
  }
}

type BaseFeeParams = {
  inverseElasticityMultiplier: bigint
  kRate: bigint
}

export const feeFixedPointScale = BigInt(10000)

export function calc1559BaseFee(
  gasUsed: bigint,
  gasLimit: bigint,
  baseFee: bigint,
  baseFeeParams?: BaseFeeParams,
): bigint {
  const { inverseElasticityMultiplier = 5000n, kRate = 200n } = baseFeeParams ?? {}
  // Calculate gasTarget: multiply first to avoid integer division truncation
  const gasTarget = (gasLimit * inverseElasticityMultiplier) / feeFixedPointScale

  if (gasUsed === gasTarget || kRate === 0n || gasTarget === 0n) return baseFee

  if (gasUsed > gasTarget) {
    const numer = baseFee * (gasUsed - gasTarget)
    const denom = (gasTarget * feeFixedPointScale) / kRate
    const delta = numer / denom // integer division (trunc)
    const inc = delta === 0n ? 1n : delta // <-- minimum +1
    return baseFee + inc
  } else {
    const numer = baseFee * (gasTarget - gasUsed)
    const denom = (gasTarget * feeFixedPointScale) / kRate
    const dec = numer / denom // integer division (trunc)
    return baseFee > dec ? baseFee - dec : 0n // saturating_sub
  }
}
