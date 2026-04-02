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
import { Address, concat, toHex } from 'viem'
import {
  schemaAddress,
  addressToBytes32,
  slotForAddressMap,
  slotIndex,
  StorageSlot,
  buildImplContractAlloc,
  buildSystemContractAlloc,
  schemaBigInt,
} from './types'
import { BuilderContext } from './context'
import { fiatTokenProxyAddress } from './addresses'

export const FIAT_TOKEN_ADDRESS = fiatTokenProxyAddress
const DEFAULT_IMPL_CONTRACT = 'NativeFiatTokenV2_2'
const DEFAULT_PROXY_CONTRACT = 'FiatTokenProxy'
// keccak256("org.zeppelinos.proxy.admin")
const FIAT_TOKEN_PROXY_ADMIN_SLOT = '0x10d6a54a4754c8869d6886b5f5d7fbfa5b4522237ea5c60d11bc4e7a1ff9390b' as const
// keccak256("org.zeppelinos.proxy.implementation")
export const FIAT_TOKEN_PROXY_IMPL_SLOT = '0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3' as const

export const schemaNativeFiatToken = z
  .object({
    proxy: z
      .object({
        address: schemaAddress.default(FIAT_TOKEN_ADDRESS).optional(),
        contractName: z.string().default(DEFAULT_PROXY_CONTRACT).optional(),
        /**
         * The admin of the proxy contract, which can upgrade the implementation contract.
         */
        admin: schemaAddress,
      })
      .strict(),
    implementation: z
      .object({
        address: schemaAddress.optional(),
        contractName: z.string().default(DEFAULT_IMPL_CONTRACT).optional(),
      })
      .strict()
      .optional(),

    owner: schemaAddress,
    pauser: schemaAddress,
    blacklister: schemaAddress,
    masterMinter: schemaAddress,
    rescuer: schemaAddress,
    minters: z.array(z.object({ address: schemaAddress, allowance: schemaBigInt }).strict()).min(1),
  })
  .strict()
  .superRefine((data, ctx) => {
    const operators = [
      ...['owner', 'pauser', 'blacklister', 'masterMinter'].map((key) => ({
        key,
        value: data[key as keyof typeof data] as Address,
      })),
      ...data.minters.map((minter, i) => ({
        key: `minters[${i}]`,
        value: minter.address,
      })),
    ]
    for (const { key, value } of operators) {
      if (value === data.proxy.admin) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Operator ${key} cannot be the same as the proxy admin`,
        })
      }
    }

    const minterSet = new Set()
    for (const minter of data.minters) {
      if (minterSet.has(minter.address)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Minter ${minter.address} must be unique`,
        })
      }
      minterSet.add(minter.address)
    }
  })

export type NativeFiatTokenConfig = z.infer<typeof schemaNativeFiatToken>

export const buildNativeFiatTokenGenesisAllocs = async (ctx: BuilderContext, config: NativeFiatTokenConfig) => {
  const {
    proxy,
    implementation: impl,
    owner,
    pauser,
    blacklister,
    masterMinter,
    rescuer,
    minters,
  } = schemaNativeFiatToken.parse(config)

  if (proxy.address != null && proxy.address !== FIAT_TOKEN_ADDRESS) {
    throw new Error('Proxy address must be FIAT_TOKEN_ADDRESS')
  }

  const [signatureCheckerAddress, signatureCheckerAlloc] = await buildImplContractAlloc(ctx, 'SignatureChecker')
  const [implAddress, implAlloc] = await buildImplContractAlloc(ctx, impl?.contractName ?? DEFAULT_IMPL_CONTRACT)
  const [proxyAddress, proxyAlloc] = await buildSystemContractAlloc({
    ctx,
    address: proxy.address ?? FIAT_TOKEN_ADDRESS,
    contractName: proxy.contractName ?? DEFAULT_PROXY_CONTRACT,
    storage: [
      StorageSlot(FIAT_TOKEN_PROXY_ADMIN_SLOT, addressToBytes32(proxy.admin)),
      StorageSlot(FIAT_TOKEN_PROXY_IMPL_SLOT, addressToBytes32(implAddress)),

      /*
       * `forge inspect NativeFiatTokenV2_2 storage`
       * ╭-------------------------------------+-------------------------------------------------+------+--------+-------╮
       * | Name                                | Type                                            | Slot | Offset | Bytes |
       * +===============================================================================================================+
       * | _owner                              | address                                         | 0    | 0      | 20    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | pauser                              | address                                         | 1    | 0      | 20    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | paused                              | bool                                            | 1    | 20     | 1     |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | blacklister                         | address                                         | 2    | 0      | 20    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _deprecatedBlacklisted              | mapping(address => bool)                        | 3    | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | name                                | string                                          | 4    | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | symbol                              | string                                          | 5    | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | decimals                            | uint8                                           | 6    | 0      | 1     |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | currency                            | string                                          | 7    | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | masterMinter                        | address                                         | 8    | 0      | 20    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | initialized                         | bool                                            | 8    | 20     | 1     |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | balanceAndBlacklistStates           | mapping(address => uint256)                     | 9    | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | allowed                             | mapping(address => mapping(address => uint256)) | 10   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | totalSupply_                        | uint256                                         | 11   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | minters                             | mapping(address => bool)                        | 12   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | minterAllowed                       | mapping(address => uint256)                     | 13   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _rescuer                            | address                                         | 14   | 0      | 20    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _DEPRECATED_CACHED_DOMAIN_SEPARATOR | bytes32                                         | 15   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _authorizationStates                | mapping(address => mapping(bytes32 => bool))    | 16   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _permitNonces                       | mapping(address => uint256)                     | 17   | 0      | 32    |
       * |-------------------------------------+-------------------------------------------------+------+--------+-------|
       * | _initializedVersion                 | uint8                                           | 18   | 0      | 1     |
       * ╰-------------------------------------+-------------------------------------------------+------+--------+-------╯
       */
      StorageSlot(slotIndex(0n), addressToBytes32(owner)),
      StorageSlot(slotIndex(1n), addressToBytes32(pauser)),
      StorageSlot(slotIndex(2n), addressToBytes32(blacklister)),
      StorageSlot(slotIndex(4n), '0x5553444300000000000000000000000000000000000000000000000000000008'), // name
      StorageSlot(slotIndex(5n), '0x5553444300000000000000000000000000000000000000000000000000000008'), // symbol
      StorageSlot(slotIndex(6n), '0x0000000000000000000000000000000000000000000000000000000000000006'), // decimals
      StorageSlot(slotIndex(7n), '0x5553440000000000000000000000000000000000000000000000000000000006'), // currency
      StorageSlot(slotIndex(8n), concat([toHex(1n, { size: 12 }), masterMinter])), // initialized, masterMinter
      // minters: mapping(address => bool)
      ...minters.map((minter) => StorageSlot(slotForAddressMap(12n, minter.address), toHex(1n, { size: 32 }))),
      // minterAllowed: mapping(address => uint256)
      ...minters.map((minter) =>
        StorageSlot(slotForAddressMap(13n, minter.address), toHex(minter.allowance, { size: 32 })),
      ),
      StorageSlot(slotIndex(14n), addressToBytes32(rescuer)),
      StorageSlot(slotIndex(18n), toHex(3n, { size: 32 })), // _initializedVersion
    ],
  })

  return {
    [proxyAddress]: proxyAlloc,
    [implAddress]: implAlloc,
    [signatureCheckerAddress]: signatureCheckerAlloc,
  }
}
