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
import {
  addressToBytes32,
  buildImplContractAlloc,
  buildSystemContractAlloc,
  schemaAddress,
  slotForAddressMap,
  slotIndex,
  StorageSlot,
  toBytes32,
} from './types'
import { BuilderContext } from './context'
import { AdminUpgradeableProxy, schemaAdminProxy, schemaAdminProxyImpl, setInitializers } from './AdminUpgradeableProxy'
import { denylistAddress } from './addresses'
import { DENYLIST_VERSION } from './versions'

const DEFAULT_PROXY_ADDRESS = denylistAddress
const DEFAULT_IMPL_CONTRACT = 'Denylist'

// ERC-7201 Storage: arc.storage.Denylist.v1
// keccak256(abi.encode(uint256(keccak256("arc.storage.Denylist.v1")) - 1)) & ~bytes32(uint256(0xff))
const DENYLIST_STORAGE_LOCATION = 0x1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00n
// DenylistStorage layout: denylisted mapping at baseSlot+0 (empty in genesis, populated by denylisters post-deployment),
//                         denylisters mapping at baseSlot+1 (initialized below if provided in config)

export const schemaDenylist = z
  .object({
    proxy: schemaAdminProxy(DEFAULT_PROXY_ADDRESS),
    implementation: schemaAdminProxyImpl(DEFAULT_IMPL_CONTRACT),

    /**
     * The owner of the Denylist contract (can add/remove denylisters).
     */
    owner: schemaAddress,

    /**
     * Optional initial denylisters. Owner can add more after genesis.
     */
    denylisters: z.array(schemaAddress).optional(),
  })
  .strict()

export type DenylistConfig = z.infer<typeof schemaDenylist>

export const buildDenylistGenesisAllocs = async (ctx: BuilderContext, config: DenylistConfig) => {
  const { proxy, implementation: impl, owner, denylisters } = schemaDenylist.parse(config)

  const [implAddress, implAlloc] = await buildImplContractAlloc(ctx, impl?.contractName ?? DEFAULT_IMPL_CONTRACT)
  const denylistersList = denylisters ?? []

  const storageSlots: ReturnType<typeof StorageSlot>[] = [
    StorageSlot(AdminUpgradeableProxy.ADMIN_SLOT, addressToBytes32(proxy.admin)),
    StorageSlot(AdminUpgradeableProxy.IMPL_SLOT, addressToBytes32(implAddress)),

    /*
     * OwnableUpgradeable -- ERC-7201 slot for openzeppelin.storage.Ownable
     * `keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff))`
     */
    StorageSlot(
      slotIndex(0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300n),
      addressToBytes32(owner),
    ),

    // Initializable
    setInitializers(DENYLIST_VERSION),

    /*
     * DenylistStorage.denylisters mapping: for each initial denylister set slot to 1 (true).
     * Slot = keccak256(abi.encode(denylisterAddress, baseSlot + 1))
     */
    ...denylistersList.map((addr) =>
      StorageSlot(slotForAddressMap(DENYLIST_STORAGE_LOCATION + 1n, addr), toBytes32(1)),
    ),
  ]

  const [proxyAddress, proxyAlloc] = await buildSystemContractAlloc({
    ctx,
    contractName: proxy?.contractName ?? AdminUpgradeableProxy.CONTRACT_NAME,
    address: proxy.address ?? DEFAULT_PROXY_ADDRESS,
    storage: storageSlots,
  })

  return {
    [implAddress]: implAlloc,
    [proxyAddress]: proxyAlloc,
  }
}
