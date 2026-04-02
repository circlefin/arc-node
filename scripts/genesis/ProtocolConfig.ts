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
  schemaBigInt,
  slotIndex,
  StorageSlot,
  toBytes32,
} from './types'
import { BuilderContext } from './context'
import { concat, toHex } from 'viem'
import { AdminUpgradeableProxy, schemaAdminProxy, schemaAdminProxyImpl, setInitializers } from './AdminUpgradeableProxy'
import { protocolConfigAddress } from './addresses'
import { PROTOCOL_CONFIG_VERSION } from './versions'

const DEFAULT_PROXY_ADDRESS = protocolConfigAddress
const DEFAULT_IMPL_CONTRACT = 'ProtocolConfig'

// ERC-7201 Storage Locations
// ProtocolConfig: `keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfig")) - 1)) & ~bytes32(uint256(0xff))`
const PROTOCOL_CONFIG_STORAGE_LOCATION = 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200n
// Controller: `keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfigController")) - 1)) & ~bytes32(uint256(0xff))`
const PROTOCOL_CONFIG_CONTROLLER_STORAGE_LOCATION = 0x958f8fec699b51a1249f513eceda5429078000657f74abd1721bba363087af00n
// Pausable: `keccak256(abi.encode(uint256(keccak256("arc.storage.Pausable")) - 1)) & ~bytes32(uint256(0xff))`
const PAUSABLE_STORAGE_LOCATION = 0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00n

const maxUint64 = 18446744073709551615n

export const schemaProtocolConfig = z
  .object({
    proxy: schemaAdminProxy(DEFAULT_PROXY_ADDRESS),
    implementation: schemaAdminProxyImpl(DEFAULT_IMPL_CONTRACT),

    /**
     * The owner of the protocol config contract, which can update the controller address.
     */
    owner: schemaAddress,

    /**
     * The controller of the protocol config contract, which can update the config.
     */
    controller: schemaAddress,

    /**
     * The pauser of the protocol config contract, which can pause the protocol.
     */
    pauser: schemaAddress,

    /**
     * The initial beneficiary, which can receive the block rewards.
     */
    beneficiary: schemaAddress,
    feeParams: z.object({
      alpha: schemaBigInt.min(0n).max(100n),
      kRate: schemaBigInt.min(0n).max(10000n),
      inverseElasticityMultiplier: schemaBigInt.min(0n).max(10000n),
      minBaseFee: schemaBigInt.min(0n).max(maxUint64),
      maxBaseFee: schemaBigInt.min(0n).max(maxUint64),
      blockGasLimit: schemaBigInt.min(0n).max(maxUint64),
    }),
    consensusParams: z
      .object({
        timeoutProposeMs: schemaBigInt,
        timeoutProposeDeltaMs: schemaBigInt,
        timeoutPrevoteMs: schemaBigInt,
        timeoutPrevoteDeltaMs: schemaBigInt,
        timeoutPrecommitMs: schemaBigInt,
        timeoutPrecommitDeltaMs: schemaBigInt,
        timeoutRebroadcastMs: schemaBigInt,
        targetBlockTimeMs: schemaBigInt,
      })
      .optional(),
  })
  .strict()

export type ProtocolConfigConfig = z.infer<typeof schemaProtocolConfig>

export const buildProtocolConfigGenesisAllocs = async (ctx: BuilderContext, config: ProtocolConfigConfig) => {
  const {
    proxy,
    implementation: impl,
    owner,
    controller,
    pauser,
    beneficiary,
    feeParams,
    consensusParams,
  } = schemaProtocolConfig.parse(config)

  const [implAddress, implAlloc] = await buildImplContractAlloc(ctx, impl?.contractName ?? DEFAULT_IMPL_CONTRACT)
  const [proxyAddress, proxyAlloc] = await buildSystemContractAlloc({
    ctx,
    contractName: proxy?.contractName ?? AdminUpgradeableProxy.CONTRACT_NAME,
    address: proxy.address ?? DEFAULT_PROXY_ADDRESS,
    storage: [
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
      setInitializers(PROTOCOL_CONFIG_VERSION),

      /*
       * ERC-7201 Controller storage
       * Slot: 0x958f8fec699b51a1249f513eceda5429078000657f74abd1721bba363087af00
       * struct ProtocolConfigControllerStorage {
       *   address controller;
       * }
       */
      StorageSlot(slotIndex(PROTOCOL_CONFIG_CONTROLLER_STORAGE_LOCATION), addressToBytes32(controller)),

      /*
       * ERC-7201 Pausable storage for ProtocolConfig
       * Slot: 0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00
       * struct PausableStorage {
       *   address pauser;  // 20 bytes, offset 0
       *   bool paused;     // 1 byte, offset 20 (packed in same slot)
       * }
       */
      StorageSlot(slotIndex(PAUSABLE_STORAGE_LOCATION), addressToBytes32(pauser)),
      // Note: pauser and paused packed in same slot. paused defaults to false (byte 20 = 0x00)

      /*
       * ERC-7201 ProtocolConfig storage
       * Slot: 0x668f09ce856848ead6cb1ddee963f15ef833cea8958030868f867aec84385200
       * Slot 0: alpha (uint64) | kRate (uint64) | inverseElasticityMultiplier (uint64) | padding (64 bits)
       * Slot 1: minBaseFee (uint256)
       * Slot 2: maxBaseFee (uint256)
       * Slot 3: blockGasLimit (uint256)
       * Slot 4: rewardBeneficiary (address)
       * Slot 5: ConsensusParams packed (8 * uint16 = 128 bits)
       */
      /*
       * structure from the storage location:
       * `keccak256(abi.encode(uint256(keccak256("arc.storage.ProtocolConfig")) - 1)) & ~bytes32(uint256(0xff))`
       */
      StorageSlot(
        slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 0n),
        concat([
          toHex(0n, { size: 8 }),
          toHex(feeParams.inverseElasticityMultiplier, { size: 8 }),
          toHex(feeParams.kRate, { size: 8 }),
          toHex(feeParams.alpha, { size: 8 }),
        ]),
      ),
      StorageSlot(slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 1n), toBytes32(feeParams.minBaseFee)),
      StorageSlot(slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 2n), toBytes32(feeParams.maxBaseFee)),
      StorageSlot(slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 3n), toBytes32(feeParams.blockGasLimit)),
      // rewardBeneficiary in slot 4
      StorageSlot(slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 4n), addressToBytes32(beneficiary)),
      // ConsensusParams packed into one slot (8 * uint16 = 128 bits) in slot 5
      ...(consensusParams
        ? [
            StorageSlot(
              slotIndex(PROTOCOL_CONFIG_STORAGE_LOCATION + 5n),
              toBytes32(
                consensusParams.timeoutProposeMs |
                  (consensusParams.timeoutProposeDeltaMs << 16n) |
                  (consensusParams.timeoutPrevoteMs << 32n) |
                  (consensusParams.timeoutPrevoteDeltaMs << 48n) |
                  (consensusParams.timeoutPrecommitMs << 64n) |
                  (consensusParams.timeoutPrecommitDeltaMs << 80n) |
                  (consensusParams.timeoutRebroadcastMs << 96n) |
                  (consensusParams.targetBlockTimeMs << 112n),
              ),
            ),
          ]
        : []),
    ],
  })

  return {
    [implAddress]: implAlloc,
    [proxyAddress]: proxyAlloc,
  }
}
