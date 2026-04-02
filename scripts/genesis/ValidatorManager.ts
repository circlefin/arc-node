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
import { localhost } from 'viem/chains'
import { permissionedManagerAddress, validatorRegistryAddress } from './addresses'
import { VALIDATOR_REGISTRY_VERSION, PERMISSIONED_VALIDATOR_MANAGER_VERSION } from './versions'

const DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS = validatorRegistryAddress
const DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT = 'ValidatorRegistry'

const DEFAULT_PERMISSIONED_PROXY_ADDRESS = permissionedManagerAddress
const DEFAULT_PERMISSIONED_IMPL_CONTRACT = 'PermissionedValidatorManager'

const UINT64_MAX = (1n << 64n) - 1n
const DEFAULT_VOTING_POWER_LIMIT = 2000n
const LOCALDEV_CHAIN_ID = localhost.id
const resolveVotingPowerLimit = (chainId: number) =>
  chainId === LOCALDEV_CHAIN_ID ? UINT64_MAX : DEFAULT_VOTING_POWER_LIMIT

export const schemaValidatorManager = z
  .object({
    proxy: schemaAdminProxy(DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS),
    implementation: schemaAdminProxyImpl(DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT),

    /**
     * owner control how can add or remove validators.
     *
     * Default is set to the proxy for PermissionedValidatorManager.
     */
    owner: schemaAddress.default(DEFAULT_PERMISSIONED_PROXY_ADDRESS).optional(),

    /**
     * The initialized validators of the ValidatorRegistry.
     */
    validators: z.array(z.object({ publicKey: schemaHex, votingPower: schemaBigInt.max(UINT64_MAX) })),

    /**
     * PermissionedValidatorManager is used to manage the ValidatorRegistry.
     * PoA version for the validator manager.
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
         * The validator registerers of the PermissionedValidatorManager.
         * Which can register new validators.
         */
        validatorRegisterers: z.array(schemaAddress),
        /**
         * The controllers of the PermissionedValidatorManager.
         * Which can control the specified validator by registration ID.
         *
         * The configuration mapping the controller to secquencial registration ID
         * started from 1.
         */
        controllers: z.array(schemaAddress),
      })
      .strict()
      .optional(),
  })
  .strict()
  .superRefine((data, ctx) => {
    // Any operators cannot be the same as the proxy admin.
    if (data.owner === data.proxy.admin) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `Operator owner cannot be the same as the proxy admin of ValidatorRegistry`,
      })
    }

    // Verify the public keys are unique.
    const publicKeySet = new Set()
    for (const validator of data.validators) {
      if (publicKeySet.has(validator.publicKey)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Public key ${validator.publicKey} must be unique`,
        })
      }
    }

    const permissionedManager = data.PermissionedValidatorManager
    if (data.owner == null && permissionedManager == null) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `Either owner or PermissionedValidatorManager must be set`,
      })
    }
    if (data.owner != null && permissionedManager != null) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `owner and PermissionedValidatorManager can not be set at the same time`,
      })
    }

    if (permissionedManager == null) {
      return
    }

    // Any operators cannot be the same as the proxy admin.
    const operators = [
      { key: 'owner', value: permissionedManager.owner },
      ...permissionedManager.validatorRegisterers.map((validatorRegisterer) => ({
        key: `validatorRegisterer[${validatorRegisterer}]`,
        value: validatorRegisterer,
      })),
      ...permissionedManager.controllers.map((controller) => ({
        key: `controller[${controller}]`,
        value: controller,
      })),
    ]
    for (const { key, value } of operators) {
      if (value === permissionedManager.proxy.admin) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Operator ${key} cannot be the same as the proxy admin of PermissionedValidatorManager`,
        })
      }
    }

    // Verify addresses are unique for different roles.
    const validatorRegistererSet = new Set()
    for (const validatorRegisterer of permissionedManager.validatorRegisterers) {
      if (validatorRegistererSet.has(validatorRegisterer)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `ValidatorRegisterer ${validatorRegisterer} must be unique`,
        })
      }
    }
    const controllerSet = new Set()
    for (const controller of permissionedManager.controllers) {
      if (controllerSet.has(controller)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: `Controller ${controller} must be unique`,
        })
      }
    }

    if (data.proxy.address != null && data.proxy.address !== DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS) {
      // the ValidatorRegistry address is hardcoded in the PermissionedValidatorManager.
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `proxy.address only supports ${DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS} when PermissionedValidatorManager enabled`,
      })
    }
  })

export type ValidatorManagerConfig = z.infer<typeof schemaValidatorManager>

export const buildValidatorManagerGenesisAllocs = async (ctx: BuilderContext, config: ValidatorManagerConfig) => {
  // keccak256(abi.encode(uint256(keccak256("arc.storage.ValidatorRegistry")) - 1)) & ~bytes32(uint256(0xff));
  const REGISTRY_STORAGE_LOCATION = 0xb58da0dce03316992faea3e12c60705b8ac05a309e27e3bc8421e5b271c9d200n

  const {
    proxy,
    implementation: impl = { contractName: DEFAULT_VALIDATOR_REGISTRY_IMPL_CONTRACT },
    owner,
    validators,
    PermissionedValidatorManager: permissionedManagerConfig,
  } = schemaValidatorManager.parse(config)

  const votingPowerLimit = resolveVotingPowerLimit(ctx.chainId)
  for (const { votingPower } of validators) {
    if (votingPower > votingPowerLimit) {
      throw new Error(
        `validator votingPower ${votingPower} exceeds votingPowerLimit ${votingPowerLimit} for chainId ${ctx.chainId}`,
      )
    }
  }

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

      // OwnableUpgradeable
      StorageSlot(
        // keccak256(abi.encode(uint256(keccak256("openzeppelin.storage.Ownable")) - 1)) & ~bytes32(uint256(0xff))
        slotIndex(0x9016d09d72d40fdae2fd8ceac6b6234c7706214fd39c1cd1e609a0528c199300n),
        addressToBytes32(owner ?? permissionedManagerConfig?.proxy.address ?? DEFAULT_PERMISSIONED_PROXY_ADDRESS),
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
    ...(await buildPermissionedValidatorManagerGenesisAllocs(
      ctx,
      permissionedManagerConfig,
      proxyAddress,
      votingPowerLimit,
    )),
  }
}

export const buildPermissionedValidatorManagerGenesisAllocs = async (
  ctx: BuilderContext,
  config: ValidatorManagerConfig['PermissionedValidatorManager'],
  validatorRegistryAddress: Address,
  votingPowerLimit: bigint,
) => {
  if (config == null) {
    return {}
  }

  if (validatorRegistryAddress !== DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS) {
    throw new Error(`validatorRegistryAddress must be ${DEFAULT_VALIDATOR_REGISTRY_PROXY_ADDRESS}`)
  }

  const { proxy, implementation: impl, owner, validatorRegisterers, controllers } = config

  // ERC-7201 storage slots for controller struct
  const CONTROLLER_STORAGE_LOCATION = 0xe90ec3add3e251bfbe914c9e482b511e91a3b187718c1dc10223f64a8a644a00n
  const CONTROLLER_REGISTRATION_SLOT = CONTROLLER_STORAGE_LOCATION
  const CONTROLLER_VOTING_POWER_LIMIT_SLOT = CONTROLLER_STORAGE_LOCATION + 1n

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

      // Controller._registrationOf mapping (EIP-7201 slot for arc.storage.PVMController)
      ...controllers.map((controller, index) => {
        // Set the controllers to sequential registration IDs, starting from 1.
        const registrationId = index + 1
        return StorageSlot(slotForAddressMap(CONTROLLER_REGISTRATION_SLOT, controller), toBytes32(registrationId))
      }),

      // Controller._votingPowerLimitOf mapping (EIP-7201 slot for arc.storage.PVMController, offset +1)
      ...controllers.map((controller) =>
        StorageSlot(slotForAddressMap(CONTROLLER_VOTING_POWER_LIMIT_SLOT, controller), toBytes32(votingPowerLimit)),
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
