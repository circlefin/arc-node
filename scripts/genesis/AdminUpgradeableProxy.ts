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
import { schemaAddress, StorageSlot } from './types'
import { Address, concat, toHex } from 'viem'

export class AdminUpgradeableProxy {
  static readonly CONTRACT_NAME = 'AdminUpgradeableProxy'

  // keccak256("eip1967.proxy.admin") - 1
  static readonly ADMIN_SLOT = '0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103' as const

  // bytes32(uint256(keccak256('eip1967.proxy.implementation')) - 1)
  static readonly IMPL_SLOT = '0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc' as const
}

/**
 * Define the schema for the AdminUpgradeableProxy.
 */
export const schemaAdminProxy = (defaultAddress: Address) =>
  z
    .object({
      address: schemaAddress.default(defaultAddress).optional(),
      contractName: z.string().default(AdminUpgradeableProxy.CONTRACT_NAME).optional(),
      /**
       * The admin of the proxy contract, which can upgrade the implementation contract.
       */
      admin: schemaAddress,
    })
    .strict()

/**
 * Define the schema for the implementation behind the AdminUpgradeableProxy.
 */
export const schemaAdminProxyImpl = (contractName: string) =>
  z
    .object({
      address: schemaAddress.optional(),
      contractName: z.string().default(contractName).optional(),
    })
    .strict()
    .optional()

/**
 * setInitializers return the storage to set the initializers to a specific version
 * The storage is defined in Initializable.
 * @param version The initialization version (e.g., 1 for initialized, 2 for upgraded once, etc.)
 *                Use 0xffffffffffffffff (u64.max) to disable initializers permanently
 */
export const setInitializers = (version: bigint) =>
  StorageSlot(
    // keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Initializable")) - 1)) & ~bytes32(uint256(0xff))
    '0xf0c57e16840df040f15088dc2f81fe391c3923bec73e23a9662efc9c229c6a00',
    concat([
      toHex(0n, { size: 24 }),
      toHex(version, { size: 8 }), // uint64 version
    ]),
  )
