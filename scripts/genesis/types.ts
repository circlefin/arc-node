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

import { z } from 'zod'
import { Address, encodePacked, fromHex, Hex, keccak256, toHex } from 'viem'
import { BuilderContext } from './context'

export type Bytes32 = `0x${string}`

export const schemaBigInt = z.coerce.bigint()
export const schemaAddress = z.string().regex(/^0x[0-9a-fA-F]{40}$/) as z.Schema<Address>
export const schemaHex = z.string().regex(/^0x([0-9a-fA-F][0-9a-fA-F])+$/) as z.Schema<Hex>
export const schemaBytes32 = z.string().regex(/^0x([0-9a-fA-F][0-9a-fA-F]){32}$/) as z.Schema<Bytes32>

export const schemaStorage = z.tuple([schemaBytes32, schemaBytes32])

export const StorageSlot = (key: Bytes32, value: Bytes32) => schemaStorage.parse([key, value])

// currentTimestamp returns the current unix timestamp, minus one second to ensure it's in the past.
export const currentTimestamp = () => BigInt(new Date().getTime()) / 1000n - 1n

export const schemaAllocConfig = z
  .object({
    address: schemaAddress,
    balance: schemaBigInt,
    nonce: schemaBigInt,
    code: schemaHex.optional(),
    storage: z.array(schemaStorage).optional(),
  })
  .strict()

export type AllocConfig = z.infer<typeof schemaAllocConfig>

export type GenesisAccountAlloc = {
  balance: Hex
  nonce?: Hex
  code?: Hex
  storage?: Partial<Record<Bytes32, Bytes32>>
}

export const buildAccountAlloc = (config: AllocConfig): [Address, GenesisAccountAlloc] => {
  const { address, balance, nonce, code, storage } = schemaAllocConfig.parse(config)
  let storageMap: Record<Bytes32, Bytes32> | undefined
  if (storage != null) {
    storageMap = {} as Record<Bytes32, Bytes32>
    for (const [key, value] of storage ?? []) {
      if (key in storageMap) {
        throw new Error(`Duplicate storage key: ${key}`)
      }
      storageMap[key] = value
    }
  }
  return [
    address,
    {
      balance: toHex(balance),
      nonce: nonce === 0n ? undefined : toHex(nonce),
      code,
      storage: storageMap,
    },
  ]
}

export const buildImplContractAlloc = async (
  ctx: BuilderContext,
  contractName: string,
  override?: Omit<Partial<AllocConfig>, 'storage'>,
) =>
  buildAccountAlloc({
    address: override?.address ?? (await ctx.contractLoader.getDeterministicAddress(contractName)),
    code: override?.code ?? (await ctx.contractLoader.getCode(contractName)),
    balance: override?.balance ?? 0n,
    nonce: override?.nonce ?? 1n,
  })

export const buildSystemContractAlloc = async ({
  ctx,
  address,
  contractName,
  ...opts
}: {
  ctx: BuilderContext
  address: Address
  contractName: string
  storage?: AllocConfig['storage']
  nonce?: AllocConfig['nonce']
  balance?: AllocConfig['balance']
}) =>
  buildAccountAlloc({
    address,
    code: await ctx.contractLoader.getCode(contractName),
    balance: opts.balance ?? 0n,
    nonce: opts.nonce ?? 1n,
    storage: opts.storage,
  })

export const addressToBigInt = (address: Address): bigint => fromHex(address, 'bigint')
export const addressToBytes32 = (address: Address): Bytes32 => toHex(addressToBigInt(address), { size: 32 })

/**
 * Convert a value to a bytes32 value.
 * @param value {boolean | number | bigint} - The value to convert.
 * @returns The bytes32 value.
 */
export const toBytes32 = (value: boolean | number | bigint): Bytes32 => {
  if (typeof value === 'boolean') {
    return toHex(value ? 1n : 0n, { size: 32 })
  }
  if (typeof value === 'number') {
    return toHex(BigInt(value), { size: 32 })
  }
  if (typeof value === 'bigint') {
    return toHex(value, { size: 32 })
  }
  // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
  throw new Error(`Invalid value: ${value}`)
}

export const slotIndex = (slotIndex: bigint): Bytes32 => toBytes32(slotIndex)

/**
 * slotForUint256Map implements the basic slot calculation for uint256 key.
 * https://docs.soliditylang.org/en/latest/internals/layout_in_storage.html#mappings-and-dynamic-arrays
 */
export const slotForUint256Map = (slotIndex: bigint, key: bigint): Bytes32 =>
  keccak256(encodePacked(['uint256', 'uint256'], [key, slotIndex]))

/**
 * The same as slotForUint256Map, but convert address to bigint first.
 */
export const slotForAddressMap = (slotIndex: bigint, address: Address): Bytes32 =>
  slotForUint256Map(slotIndex, addressToBigInt(address))

/**
 * The same as slotForUint256Map, but convert bytes32 to bigint first.
 */
export const slotForBytes32Map = (slotIndex: bigint, key: Bytes32): Bytes32 =>
  slotForUint256Map(slotIndex, fromHex(schemaBytes32.parse(key), 'bigint'))

/**
 * bigintReplacer encode bigint to string.
 *
 * Example:
 * ```typescript
 * JSON.stringify(obj, bigintReplacer, 2);
 * ```
 */
export const bigintReplacer = (_: unknown, v: unknown) => (typeof v === 'bigint' ? v.toString() : v)
