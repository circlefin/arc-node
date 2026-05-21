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
  enforceOperatorsNotProxyAdmin,
  schemaAddress,
  schemaBigInt,
  schemaHex,
  slotForAddressMap,
  slotForBytes32Map,
  slotIndex,
  StorageSlot,
  toBytes32,
} from './types'
import { BuilderContext } from './context'
import { AdminUpgradeableProxy, schemaAdminProxy, schemaAdminProxyImpl, setInitializers } from './AdminUpgradeableProxy'
import { Address, fromHex, keccak256 } from 'viem'
import { permissionedManagerAddress, validatorRegistryAddress } from './addresses'
import { VALIDATOR_REGISTRY_VERSION, PERMISSIONED_VALIDATOR_MANAGER_VERSION } from './versions'

const DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS = validatorRegistryAddress
const DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT = 'ValidatorRegistry'

const DEFAULT_PERMISSIONED_PROXY_ADDRESS = permissionedManagerAddress
const DEFAULT_PERMISSIONED_IMPL_CONTRACT = 'PermissionedValidatorManager'

const UINT64_MAX = (1n << 64n) - 1n

export const schemaValidatorManager = z
  .object({
    proxy: schemaAdminProxy(DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS),
    implementation: schemaAdminProxyImpl(DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT),

    /**
     * The initialized validators of the ValidatorRegistry. The 1-based index
     * of each entry is its registration ID on-chain.
     */
    validators: z.array(
      z.object({
        publicKey: schemaHex,
        votingPower: schemaBigInt.max(UINT64_MAX),
        /**
         * Controllers authorized to manage this validator. Each controller is
         * wired at genesis to the validator's registration ID (the 1-based
         * index in `validators`).
         */
        controllers: z
          .array(
            z
              .object({
                address: schemaAddress,
                votingPowerLimit: schemaBigInt.max(UINT64_MAX),
              })
              .strict(),
          )
          .min(1),
      }),
    ),

    /**
     * PermissionedValidatorManager manages the ValidatorRegistry via PoA governance.
     */
    PermissionedValidatorManager: z
      .object({
        proxy: schemaAdminProxy(DEFAULT_PERMISSIONED_PROXY_ADDRESS),
        implementation: schemaAdminProxyImpl(DEFAULT_PERMISSIONED_IMPL_CONTRACT),
        /**
         * The owner of the PermissionedValidatorManager.
         */
        owner: schemaAddress,
        /**
         * The pauser of the PermissionedValidatorManager, which can pause/unpause
         * validator registration and controller operations.
         */
        pauser: schemaAddress,
        /**
         * The validator registerers of the PermissionedValidatorManager.
         * Which can register new validators.
         */
        validatorRegisterers: z.array(schemaAddress),
      })
      .strict(),
  })
  .strict()
  .superRefine((data, ctx) => {
    // Verify the public keys are unique.
    const publicKeySet = new Set()
    for (const validator of data.validators) {
      if (publicKeySet.has(validator.publicKey)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Public key ${validator.publicKey} must be unique`,
        })
      }
      publicKeySet.add(validator.publicKey)
    }

    const permissionedManager = data.PermissionedValidatorManager

    const flattenedControllers: Array<{ address: Address; key: string }> = []
    data.validators.forEach((validator, i) => {
      validator.controllers.forEach((c, j) => {
        flattenedControllers.push({ address: c.address, key: `validators[${i}].controllers[${j}]` })
      })
    })

    enforceOperatorsNotProxyAdmin(ctx, 'PermissionedValidatorManager', permissionedManager.proxy.admin, [
      { key: 'owner', value: permissionedManager.owner },
      { key: 'pauser', value: permissionedManager.pauser },
      ...permissionedManager.validatorRegisterers.map((validatorRegisterer) => ({
        key: `validatorRegisterer[${validatorRegisterer}]`,
        value: validatorRegisterer,
      })),
      ...flattenedControllers.map(({ key, address }) => ({ key, value: address })),
    ])

    // Verify addresses are unique for different roles.
    const validatorRegistererSet = new Set()
    for (const validatorRegisterer of permissionedManager.validatorRegisterers) {
      if (validatorRegistererSet.has(validatorRegisterer)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `ValidatorRegisterer ${validatorRegisterer} must be unique`,
        })
      }
      validatorRegistererSet.add(validatorRegisterer)
    }
    const controllerSet = new Set<Address>()
    for (const { address, key } of flattenedControllers) {
      if (controllerSet.has(address)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Controller ${address} (${key}) must be unique across all validators`,
        })
      }
      controllerSet.add(address)
    }

    if (data.proxy.address != null && data.proxy.address !== DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS) {
      // the ValidatorRegistry address is hardcoded in the PermissionedValidatorManager.
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `proxy.address only supports ${DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS}`,
      })
    }

    // The PVM proxy address becomes the ValidatorRegistry's owner at genesis,
    // so it must not also be the registry's proxy admin — otherwise one entity
    // would simultaneously own the registry's logic and control its upgrades.
    const pvmProxyAddress = permissionedManager.proxy.address ?? DEFAULT_PERMISSIONED_PROXY_ADDRESS
    enforceOperatorsNotProxyAdmin(ctx, 'ValidatorRegistry', data.proxy.admin, [
      { key: 'owner', value: pvmProxyAddress },
    ])
  })

export type ValidatorManagerConfig = z.infer<typeof schemaValidatorManager>

export const buildValidatorManagerGenesisAllocs = async (ctx: BuilderContext, config: ValidatorManagerConfig) => {
  // keccak256(abi.encode(uint256(keccak256("arc.storage.ValidatorRegistry")) - 1)) & ~bytes32(uint256(0xff));
  const REGISTRY_STORAGE_LOCATION = 0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200n

  const {
    proxy,
    implementation: impl = { contractName: DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT },
    validators,
    PermissionedValidatorManager: permissionedManagerConfig,
  } = schemaValidatorManager.parse(config)

  const [implAddress, implAlloc] = await buildImplContractAlloc(
    ctx,
    impl?.contractName ?? DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT,
  )
  const [proxyAddress, proxyAlloc] = await buildSystemContractAlloc({
    ctx,
    address: proxy.address ?? DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS,
    contractName: proxy?.contractName ?? AdminUpgradeableProxy.CONTRACT_NAME,
    storage: [
      StorageSlot(AdminUpgradeableProxy.ADMIN_SLOT, addressToBytes32(proxy.admin)),
      StorageSlot(AdminUpgradeableProxy.IMPL_SLOT, addressToBytes32(implAddress)),

      // Initializable
      setInitializers(VALIDATOR_REGISTRY_VERSION),

      StorageSlot(
        // keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff))
        slotIndex(0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300n),
        addressToBytes32(permissionedManagerConfig.proxy.address ?? DEFAULT_PERMISSIONED_PROXY_ADDRESS),
      ),

      // ValidatorRegistryStorage
      ...validators.flatMap((validator, index) => {
        const registrationId = toBytes32(BigInt(index) + 1n) // start from 1
        const idSetArraySlot = fromHex(keccak256(slotIndex(REGISTRY_STORAGE_LOCATION + 1n)), 'bigint') + BigInt(index)
        const idSetMapSlotHex = slotForBytes32Map(REGISTRY_STORAGE_LOCATION + 2n, registrationId)
        const validatorSlot = fromHex(slotForBytes32Map(REGISTRY_STORAGE_LOCATION + 0n, registrationId), 'bigint')
        const publicKeyLength = BigInt(fromHex(validator.publicKey, 'bytes').length)

        if (publicKeyLength !== 32n) {
          // Only support 32 bytes public key now.
          throw new Error(`Public key must be 32 bytes`)
        }

        return [
          // _validatorsByRegistrationId, mapping(uint256 => Validator = (enum, bytes, uint64))
          StorageSlot(slotIndex(validatorSlot), toBytes32(2)), // status: Active
          StorageSlot(slotIndex(validatorSlot + 1n), toBytes32(publicKeyLength * 2n + 1n)), // encode length
          StorageSlot(keccak256(slotIndex(validatorSlot + 1n)), validator.publicKey), // 32 bytes public key
          StorageSlot(slotIndex(validatorSlot + 2n), toBytes32(validator.votingPower)),

          // _activeValidatorRegistrations, EnumerableSet.UintSet = (bytes32[], mapping(bytes32 value => uint256))
          // - Set the array slot to the registration ID.
          StorageSlot(slotIndex(idSetArraySlot), registrationId),
          // - Mapping registration ID to the array index + 1.
          StorageSlot(idSetMapSlotHex, toBytes32(index + 1)),

          // _registeredPublicKeys, mapping(bytes32 => bool)
          StorageSlot(slotForBytes32Map(REGISTRY_STORAGE_LOCATION + 3n, keccak256(validator.publicKey)), toBytes32(1n)),
        ]
      }),
      // ValidatorRegistryStorage._activeValidatorRegistrations._values.length
      StorageSlot(slotIndex(REGISTRY_STORAGE_LOCATION + 1n), toBytes32(validators.length)),
      // ValidatorRegistryStorage._nextRegistrationID, uint256
      StorageSlot(slotIndex(REGISTRY_STORAGE_LOCATION + 4n), toBytes32(validators.length + 1)),
    ],
  })

  return {
    [implAddress]: implAlloc,
    [proxyAddress]: proxyAlloc,
    ...(await buildPermissionedValidatorManagerGenesisAllocs(ctx, permissionedManagerConfig, validators, proxyAddress)),
  }
}

export const buildPermissionedValidatorManagerGenesisAllocs = async (
  ctx: BuilderContext,
  config: ValidatorManagerConfig['PermissionedValidatorManager'],
  validators: ValidatorManagerConfig['validators'],
  validatorRegistryAddress: Address,
) => {
  if (validatorRegistryAddress !== DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS) {
    throw new Error(`validatorRegistryAddress must be ${DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS}`)
  }

  const { proxy, implementation: impl, owner, pauser, validatorRegisterers } = config

  // Flatten controllers from each validator. registrationId = validator index + 1.
  const flattenedControllers = validators.flatMap((validator, index) =>
    validator.controllers.map((c) => ({
      address: c.address,
      registrationId: BigInt(index + 1),
      votingPowerLimit: c.votingPowerLimit,
    })),
  )

  // ERC-7201 storage slots for controller struct
  const CONTROLLER_STORAGE_LOCATION = 0xe90ec3add3e251bfbe914c9e482b511e91a3b187718c1dc10223f64a8a644a00n
  const CONTROLLER_REGISTRATION_SLOT = CONTROLLER_STORAGE_LOCATION
  const CONTROLLER_VOTING_POWER_LIMIT_SLOT = CONTROLLER_STORAGE_LOCATION + 1n

  // ERC-7201 Pausable storage location (shared with all Pausable-derived contracts)
  // keccak256(abi.encode(uint256(keccak256("arc.storage.Pausable")) - 1)) & ~bytes32(uint256(0xff))
  const PAUSABLE_STORAGE_LOCATION = 0x0642d7922329a434cf4fd17a3c95eb692c24fd95f9f94d0b55420a5d895f4a00n

  const [implAddress, implAlloc] = await buildImplContractAlloc(
    ctx,
    impl?.contractName ?? DEFAULT_PERMISSIONED_IMPL_CONTRACT,
  )
  const [proxyAddress, proxyAlloc] = await buildSystemContractAlloc({
    ctx,
    address: proxy.address ?? DEFAULT_PERMISSIONED_PROXY_ADDRESS,
    contractName: proxy?.contractName ?? AdminUpgradeableProxy.CONTRACT_NAME,
    storage: [
      StorageSlot(AdminUpgradeableProxy.ADMIN_SLOT, addressToBytes32(proxy.admin)),
      StorageSlot(AdminUpgradeableProxy.IMPL_SLOT, addressToBytes32(implAddress)),

      // Initializable
      setInitializers(PERMISSIONED_VALIDATOR_MANAGER_VERSION),

      /**
       * EIP-7201 Storage Locations:
       * - Ownable: 0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300
       * - PVMController: 0xe90ec3add3e251bfbe914c9e482b511e91a3b187718c1dc10223f64a8a644a00
       * - PVMValidatorRegisterer: 0x36c39aeb5f498ae36546fc14573b003abf87227a5a2df6caec16ee566f1ad800
       */

      // OwnableUpgradeable._owner
      StorageSlot(
        slotIndex(0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300n),
        addressToBytes32(owner),
      ),

      // PausableStorage: pauser (20 bytes) + paused (1 byte) packed in one slot.
      // paused defaults to false (zero byte at offset 20).
      StorageSlot(slotIndex(PAUSABLE_STORAGE_LOCATION), addressToBytes32(pauser)),

      // Controller._registrationOf mapping: address → registrationId of the
      // validator it manages.
      ...flattenedControllers.map((controller) =>
        StorageSlot(
          slotForAddressMap(CONTROLLER_REGISTRATION_SLOT, controller.address),
          toBytes32(controller.registrationId),
        ),
      ),

      // Controller._votingPowerLimitOf mapping (offset +1).
      ...flattenedControllers.map((controller) =>
        StorageSlot(
          slotForAddressMap(CONTROLLER_VOTING_POWER_LIMIT_SLOT, controller.address),
          toBytes32(controller.votingPowerLimit),
        ),
      ),

      // ValidatorRegisterer._validatorRegisterers mapping (EIP-7201 slot for arc.storage.PVMValidatorRegisterer)
      ...validatorRegisterers.map((validatorRegisterer) =>
        StorageSlot(
          slotForAddressMap(0x36c39aeb5f498ae36546fc14573b003abf87227a5a2df6caec16ee566f1ad800n, validatorRegisterer),
          toBytes32(1n),
        ),
      ),
    ],
  })

  return {
    [implAddress]: implAlloc,
    [proxyAddress]: proxyAlloc,
  }
}
