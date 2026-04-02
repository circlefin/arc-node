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

import { task } from 'hardhat/config'
import { getChain } from '../viem-helper'
import { fromHex, Hex } from 'viem'
import { BlockTag } from 'viem'

/**
 * parseBlockId is a helper function to parse the block number or tag or hash into a viem block object.
 * It use the same format as the go-ethereum rpc such as eth_getBlockReceipts.
 * - 66 bytes string with 0x prefix will be parsed as block hash
 * - number or other hex string will be parsed as block number
 * - support block tag latest, earliest, pending, safe, finalized
 * ref: https://github.com/ethereum/go-ethereum/blob/15ff378a8927eed211589bcf375aa5c528209b71/rpc/types.go#L152
 * @param id - block number or tag or hash
 * @returns a viem block object
 */
function parseBlockId(id: string) {
  if (/^0x[0-9a-fA-F]{64}$/.test(id)) {
    return { blockHash: id as Hex }
  }
  if (/^[0-9]+$/.test(id)) {
    return { blockNumber: BigInt(id) }
  }
  if (/^0x[0-9a-fA-F]+$/.test(id)) {
    return { blockNumber: fromHex(id as Hex, 'bigint') }
  }
  switch (id) {
    case 'latest':
    case 'earliest':
    case 'pending':
    case 'safe':
    case 'finalized':
      return { blockTag: id as BlockTag }
  }
  throw new Error(`Invalid block id: ${id}`)
}

task('query-fee', 'query fee for the block')
  .addOptionalParam('block', 'block height or tag or hash to query', 'latest')
  .addOptionalParam('since', 'start from this block height, default to block height - 1', undefined)
  .setAction(
    async (
      { since, block: id }: { since?: string; block: string; inverseElasticityMultiplier: number; kRate: number },
      hre,
    ) => {
      const { getNextBaseFee } = await import('../../../tests/helpers')

      const client = await hre.viem.getPublicClient({ chain: getChain(hre) })

      const block = await client.getBlock(parseBlockId(id))
      if (block.number == null) {
        throw new Error('block height not found')
      }
      let height = BigInt(since ?? (block.number > 0n ? block.number - 1n : block.number))
      if (height < 0n) {
        height = block.number + height
      }
      const endBlockHeight = block.number

      const parseExtra = (extraData: Hex) => {
        if (/^0x[0-9a-fA-F]{16}$/.test(extraData)) {
          return fromHex(extraData, 'bigint')
        }
        return undefined
      }
      const dumpFeeValues = (info: Awaited<ReturnType<typeof getNextBaseFee>>) => {
        console.log(
          `${info.block.number} baseFee=${info.block.baseFeePerGas}, gasUsed/limit=${info.block.gasUsed}/${info.block.gasLimit}, hash=${info.block.hash}
    systemAccounting: gasUsed=${info.gasValues.gasUsed}, gasUsedSmoothed=${info.gasValues.gasUsedSmoothed} nextBaseFee=${info.gasValues.nextBaseFee}
    feeParams: min/max=${info.feeParams?.minBaseFee}/${info.feeParams?.maxBaseFee}, gasLimit=${info.feeParams?.blockGasLimit}, kRate=${info.feeParams?.kRate}, IEM=${info.feeParams?.inverseElasticityMultiplier}, alpha=${info.feeParams?.alpha}
    => calculated next: baseFee=${info.next.baseFeePerGas}, eip1559baseFee=${info.next.eip1559BaseFee}, baseFeeInExtra=${parseExtra(info.block.extraData)}`,
        )
      }

      let parent = await getNextBaseFee(client, height)
      dumpFeeValues(parent)
      for (height = height + 1n; height <= endBlockHeight; height++) {
        const curr = await getNextBaseFee(client, height)
        dumpFeeValues(curr)
        if (curr.block.gasUsed !== curr.gasValues.gasUsed) {
          console.error('!! gasUsed mismatch')
        }
        if (curr.block.baseFeePerGas !== parent.next.baseFeePerGas) {
          console.error('!! baseFee mismatch')
        }
        if (parent.feeParams != null) {
          if (curr.block.gasLimit !== parent.feeParams.blockGasLimit) {
            console.error('!! gasLimit mismatch')
          }
        }
        const baseFeeInExtra = parseExtra(parent.block.extraData)
        if (baseFeeInExtra != null) {
          if (baseFeeInExtra !== parent.next.baseFeePerGas) {
            console.error('!! basefee from extra mismatch')
          }
        }
        parent = curr
      }
    },
  )
