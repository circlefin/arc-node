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

import fs from 'fs'
import { task, types } from 'hardhat/config'
import { getChain } from '../viem-helper'
import { USDC } from '../../../tests/helpers/FiatToken'
import {
  GenesisConfig,
  permissionedManagerAddress,
  protocolConfigAddress,
  schemaGenesisConfig,
  validatorRegistryAddress,
  fiatTokenProxyAddress,
} from '../../genesis'
import { GenesisAccountAlloc } from '../../genesis/types'
import { FIAT_TOKEN_PROXY_IMPL_SLOT } from '../../genesis/NativeFiatToken'
import { AdminUpgradeableProxy } from '../../genesis/AdminUpgradeableProxy'
import { Address, getContract, Hex, PublicClient } from 'viem'
import { HardhatRuntimeEnvironment } from 'hardhat/types'
import { expect } from 'chai'
import path from 'path'
import { getAddress } from 'viem'

const jsonHelper = (_: unknown, v: unknown) => (typeof v === 'bigint' ? v.toString() : v)

/**
 * Load genesis.json file based on network name
 */
const loadGenesisFile = (networkName: string): Record<string, GenesisAccountAlloc> => {
  const genesisPath = path.join(__dirname, `../../../assets/${networkName}/genesis.json`)
  const genesisData = fs.readFileSync(genesisPath, 'utf-8')
  const genesis = JSON.parse(genesisData) as { alloc?: Record<string, GenesisAccountAlloc> }

  if (!genesis.alloc || typeof genesis.alloc !== 'object') {
    throw new Error('Invalid genesis file format: missing or invalid alloc section')
  }
  return genesis.alloc
}

/**
 * Common helper to get implementation address from proxy storage
 */
const getImplementationAddress = async (
  client: PublicClient,
  proxyAddress: Address,
  implSlot: Hex = AdminUpgradeableProxy.IMPL_SLOT,
): Promise<Address> => {
  const implAddressData = await client.getStorageAt({
    address: proxyAddress,
    slot: implSlot,
  })

  if (!implAddressData) {
    throw new Error(`Implementation address not found in proxy storage for ${proxyAddress}`)
  }

  // Convert storage data to address (last 20 bytes)
  const addressPart = implAddressData.substring(implAddressData.length - 40)
  return ('0x' + addressPart) as Address
}

const toJsonString = (v: unknown) => JSON.stringify(v, jsonHelper, 0)

task('query-state', 'query state for the network')
  .addOptionalParam('genesisConfig', 'genesis config file to compare with the state', undefined, types.string)
  .addOptionalParam('minter', 'additional minter address to lookup', undefined, types.string)
  .setAction(async ({ genesisConfig, minter }: { genesisConfig?: string; minter?: string }, hre) => {
    let config: GenesisConfig | undefined
    if (genesisConfig != null) {
      config = schemaGenesisConfig.parse(JSON.parse(fs.readFileSync(genesisConfig, 'utf-8')))
    }

    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })

    const state: Partial<GenesisConfig> = {
      NativeFiatToken: await queryFiatTokenConfig(hre, client, [
        ...(minter ? [getAddress(minter)] : []),
        ...(config?.NativeFiatToken.minters ?? []).map((x) => x.address),
      ]),
      ProtocolConfig: await queryProtocolConfig(hre, client),
      ValidatorManager: await queryValidatorManagerConfig(
        hre,
        client,
        config?.ValidatorManager.PermissionedValidatorManager?.validatorRegisterers,
        config?.ValidatorManager.validators,
      ),
      prefund: await queryPrefund(client, config?.prefund),
    }

    console.log(JSON.stringify(state, jsonHelper, 2))

    if (config) {
      const cmp = new ConfigComparator()
      // check implementation
      cmp.eq('NativeFiatToken.proxy.admin', state, config)
      cmp.eq('NativeFiatToken.owner', state, config)
      cmp.eq('NativeFiatToken.pauser', state, config)
      cmp.eq('NativeFiatToken.masterMinter', state, config)
      cmp.eq('NativeFiatToken.rescuer', state, config)
      cmp.eq('NativeFiatToken.blacklister', state, config)
      cmp.eq('NativeFiatToken.minters', state, config, (x: Array<{ address: Address }>) => x.map((m) => m.address))

      // Load genesis allocations once for all bytecode verifications
      const genesisAllocs = loadGenesisFile(hre.network.name)

      // Verify NativeFiatToken bytecode
      await cmp.verifyProxyCode(genesisAllocs, client, fiatTokenProxyAddress, 'NativeFiatToken')
      await cmp.verifyImplementationCode(
        genesisAllocs,
        client,
        fiatTokenProxyAddress,
        'NativeFiatToken',
        FIAT_TOKEN_PROXY_IMPL_SLOT,
      )

      cmp.eq('ProtocolConfig.proxy.admin', state, config)
      cmp.eq('ProtocolConfig.owner', state, config)
      cmp.eq('ProtocolConfig.controller', state, config)
      cmp.eq('ProtocolConfig.pauser', state, config)
      cmp.eq('ProtocolConfig.feeParams', state, config)

      // Verify ProtocolConfig bytecode
      await cmp.verifyProxyCode(genesisAllocs, client, protocolConfigAddress, 'ProtocolConfig')
      await cmp.verifyImplementationCode(genesisAllocs, client, protocolConfigAddress, 'ProtocolConfig')

      cmp.eq('ValidatorManager.proxy.admin', state, config)
      cmp.eq('ValidatorManager.PermissionedValidatorManager.proxy.admin', state, config)
      cmp.eq('ValidatorManager.PermissionedValidatorManager.owner', state, config)
      cmp.eq('ValidatorManager.PermissionedValidatorManager.pauser', state, config)
      cmp.eq('ValidatorManager.PermissionedValidatorManager.validatorRegisterers', state, config)

      // Structural invariant: ValidatorRegistry.owner == PVM proxy address. Set at
      // genesis and never changed via config, so it lives outside the schema — read
      // and compare directly so a drifted owner can't go undetected.
      const expectedVrOwner =
        config.ValidatorManager.PermissionedValidatorManager.proxy.address ?? permissionedManagerAddress
      const validatorRegistry = getContract({
        abi: hre.artifacts.readArtifactSync('ValidatorRegistry').abi,
        address: validatorRegistryAddress,
        client,
      }).read
      const vrOwner = await validatorRegistry.owner()
      if (vrOwner.toLowerCase() !== expectedVrOwner.toLowerCase()) {
        console.warn(`ValidatorRegistry.owner is ${vrOwner}, expected ${expectedVrOwner} (PVM proxy)`)
        cmp.hasDiff = true
      }
      // Compare the full nested validator shape: publicKey, votingPower, and each
      // validator's controllers (address + votingPowerLimit). The registrationId is
      // implicit (= validator index + 1) and enforced at genesis-build time.
      cmp.eq(
        'ValidatorManager.validators',
        state,
        config,
        (
          x: Array<{
            publicKey: Hex
            votingPower: bigint
            controllers?: Array<{ address: Address; votingPowerLimit: bigint }>
          }>,
        ) =>
          x.map((v) => ({
            publicKey: v.publicKey,
            votingPower: v.votingPower,
            controllers: (v.controllers ?? []).map((c) => ({
              address: c.address,
              votingPowerLimit: c.votingPowerLimit,
            })),
          })),
      )

      // Verify ValidatorManager bytecode
      await cmp.verifyProxyCode(genesisAllocs, client, validatorRegistryAddress, 'ValidatorManager')
      await cmp.verifyImplementationCode(genesisAllocs, client, validatorRegistryAddress, 'ValidatorManager')
      await cmp.verifyProxyCode(genesisAllocs, client, permissionedManagerAddress, 'PermissionedValidatorManager')
      await cmp.verifyImplementationCode(
        genesisAllocs,
        client,
        permissionedManagerAddress,
        'PermissionedValidatorManager',
      )

      cmp.eq('prefund', state, config)

      if (cmp.hasDiff) {
        throw new Error(`state and config is different`)
      }
    }
  })

const queryFiatTokenConfig = async (
  hre: HardhatRuntimeEnvironment,
  client: PublicClient,
  minterCandidates?: Array<Address>,
): Promise<GenesisConfig['NativeFiatToken']> => {
  const usdc = USDC.attach(client).read

  const [admin, impl, owner, pauser, masterMinter, rescuer, blacklister, ...minterAllowance] = await Promise.all([
    usdc.admin(),
    usdc.implementation(),
    usdc.owner(),
    usdc.pauser(),
    usdc.masterMinter(),
    usdc.rescuer(),
    usdc.blacklister(),
    ...(minterCandidates ?? []).map((address) =>
      usdc.minterAllowance([address]).then((allowance) => ({ address, allowance })),
    ),
  ])

  return {
    proxy: { admin },
    implementation: { address: impl },
    owner,
    pauser,
    masterMinter,
    rescuer,
    blacklister,
    minters: minterAllowance,
  }
}

const queryProtocolConfig = async (
  hre: HardhatRuntimeEnvironment,
  client: PublicClient,
): Promise<GenesisConfig['ProtocolConfig']> => {
  const abiProtocolConfig = hre.artifacts.readArtifactSync('ProtocolConfig').abi
  const abiAdminProxy = hre.artifacts.readArtifactSync('AdminUpgradeableProxy').abi
  const protocolConfig = getContract({
    abi: [...abiProtocolConfig, ...abiAdminProxy],
    address: protocolConfigAddress,
    client,
  }).read

  const [admin, impl, owner, controller, pauser, feeParams, consensusParams] = await Promise.all([
    protocolConfig.admin(),
    protocolConfig.implementation(),
    protocolConfig.owner(),
    protocolConfig.controller(),
    protocolConfig.pauser(),
    protocolConfig.feeParams(),
    protocolConfig.consensusParams().then((params) => ({
      timeoutProposeMs: BigInt(params.timeoutProposeMs),
      timeoutProposeDeltaMs: BigInt(params.timeoutProposeDeltaMs),
      timeoutPrevoteMs: BigInt(params.timeoutPrevoteMs),
      timeoutPrevoteDeltaMs: BigInt(params.timeoutPrevoteDeltaMs),
      timeoutPrecommitMs: BigInt(params.timeoutPrecommitMs),
      timeoutPrecommitDeltaMs: BigInt(params.timeoutPrecommitDeltaMs),
      timeoutRebroadcastMs: BigInt(params.timeoutRebroadcastMs),
      targetBlockTimeMs: BigInt(params.targetBlockTimeMs),
    })),
  ])

  return {
    proxy: { admin },
    implementation: { address: impl },
    owner,
    controller,
    pauser,
    feeParams,
    consensusParams,
  }
}

const queryValidatorManagerConfig = async (
  hre: HardhatRuntimeEnvironment,
  client: PublicClient,
  validatorRegisterers?: Array<Address>,
  configValidators?: Array<{ controllers?: Array<{ address: Address }> }>,
): Promise<GenesisConfig['ValidatorManager']> => {
  const abiValidatorRegistry = hre.artifacts.readArtifactSync('ValidatorRegistry').abi
  const abiPermissionedValidatorManager = hre.artifacts.readArtifactSync('PermissionedValidatorManager').abi
  const abiAdminProxy = hre.artifacts.readArtifactSync('AdminUpgradeableProxy').abi

  const validatorRegistry = getContract({
    abi: [...abiValidatorRegistry, ...abiAdminProxy],
    address: validatorRegistryAddress,
    client,
  }).read
  const poaManager = getContract({
    abi: [...abiPermissionedValidatorManager, ...abiAdminProxy],
    address: permissionedManagerAddress,
    client,
  }).read

  const [vrAdmin, vrImpl, nextRegistrationId, admin, impl, owner, pauser, ...isValidatorRegisterer] = await Promise.all(
    [
      validatorRegistry.admin(),
      validatorRegistry.implementation(),
      validatorRegistry.getNextRegistrationId(),
      poaManager.admin(),
      poaManager.implementation(),
      poaManager.owner(),
      poaManager.pauser(),
      ...(validatorRegisterers ?? []).map((x) => poaManager.isValidatorRegisterer([x])),
    ],
  )

  const onChainValidators = await Promise.all(
    Array(Number(nextRegistrationId - 1n))
      .fill(0)
      .map((_, i) => validatorRegistry.getValidator([BigInt(i) + 1n])),
  )

  // For each validator, query the on-chain state for the controller addresses
  // that config says should manage it. A controller is considered to belong to
  // validator i if its on-chain registrationId equals i+1 — mismatches produce
  // a missing entry here, which the comparator flags as a diff.
  const validators = await Promise.all(
    onChainValidators.map(async (v, i) => {
      const expectedRegistrationId = BigInt(i + 1)
      const configControllers = configValidators?.[i]?.controllers ?? []
      const controllers = await Promise.all(
        configControllers.map(async (c) => {
          const [registrationId, votingPowerLimit] = await Promise.all([
            poaManager.getRegistrationId([c.address]),
            poaManager.getVotingPowerLimit([c.address]),
          ])
          return registrationId === expectedRegistrationId ? { address: c.address, votingPowerLimit } : null
        }),
      )
      return {
        ...v,
        controllers: controllers.filter((c): c is { address: Address; votingPowerLimit: bigint } => c !== null),
      }
    }),
  )

  return {
    proxy: { admin: vrAdmin },
    implementation: { address: vrImpl },

    PermissionedValidatorManager: {
      proxy: { admin },
      implementation: { address: impl },
      owner,
      pauser,
      validatorRegisterers: (validatorRegisterers ?? [])?.filter((_, i) => isValidatorRegisterer[i]),
    },

    validators,
  }
}

// Read current on-chain balance for each prefund address and flag drift.
const queryPrefund = async (
  client: PublicClient,
  configPrefund: GenesisConfig['prefund'] = [],
): Promise<GenesisConfig['prefund']> =>
  Promise.all(
    configPrefund.map(async ({ address }) => ({
      address,
      balance: await client.getBalance({ address }),
    })),
  )

class ConfigComparator {
  hasDiff = false

  private deref = (path: string, x: unknown) => {
    const toks = path.split('.')
    let jsonPath = '.'
    for (let i = 0; i < toks.length; i++) {
      const key = toks[i]
      jsonPath = toks.slice(0, i + 1).join('.')

      if (x != null && typeof x === 'object' && key in x) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any,@typescript-eslint/no-unsafe-member-access
        x = (x as any)[key]
      } else {
        throw new Error(`${jsonPath}: ${key} for ${toJsonString(x)} is not an object`)
      }
    }
    return x
  }

  eq = <T>(path: string, x: unknown, y: unknown, transform: (v: T) => unknown = (v) => v) => {
    if (x === y) {
      return true
    }
    let a: unknown, b: unknown
    try {
      a = transform(this.deref(path, x) as T)
      b = transform(this.deref(path, y) as T)
    } catch (err) {
      console.warn(err)
      this.hasDiff = true
      return false
    }
    // after mapping, value should not be undefined
    if (a === undefined) {
      console.log(`${path} of ${toJsonString(x)} is undefined after tranform`)
      this.hasDiff = true
      return false
    }
    if (b === undefined) {
      console.log(`${path} of ${toJsonString(y)} is undefined after tranform`)
      this.hasDiff = true
      return false
    }
    try {
      expect(a).to.be.deep.eq(b)
      return true
    } catch (err) {
      console.warn(err)
      this.hasDiff = true
      return false
    }
  }

  verifyProxyCode = async (
    genesisAllocs: Record<string, GenesisAccountAlloc>,
    client: PublicClient,
    proxyAddress: Address,
    contractType: string,
  ) => {
    const deployedCode = await client.getCode({ address: proxyAddress })
    const proxyAlloc = genesisAllocs[proxyAddress]

    if (!proxyAlloc || !proxyAlloc.code) {
      console.warn(`No bytecode found in ${contractType} proxy allocation for address ${proxyAddress}`)
      this.hasDiff = true
      return
    }

    if (deployedCode !== proxyAlloc.code) {
      console.warn(`${contractType} proxy bytecode does not match genesis configuration for address ${proxyAddress}`)
      this.hasDiff = true
      return
    }
  }

  verifyImplementationCode = async (
    genesisAllocs: Record<string, GenesisAccountAlloc>,
    client: PublicClient,
    proxyAddress: Address,
    contractType: string,
    implSlot?: Hex,
  ) => {
    const implAddress = await getImplementationAddress(client, proxyAddress, implSlot)
    const deployedImplCode = await client.getCode({ address: implAddress })

    if (!deployedImplCode) {
      console.warn(`No bytecode found at ${contractType} implementation address ${implAddress}`)
      this.hasDiff = true
      return
    }

    // Find implementation allocation using case-insensitive address lookup
    const matchingKey = Object.keys(genesisAllocs).find((key) => key.toLowerCase() === implAddress.toLowerCase())
    const implAlloc = matchingKey ? genesisAllocs[matchingKey] : undefined

    if (!implAlloc || !implAlloc.code) {
      console.warn(`No bytecode found in ${contractType} implementation allocation for address ${implAddress}`)
      this.hasDiff = true
      return
    }

    if (deployedImplCode !== implAlloc.code) {
      console.warn(
        `${contractType} implementation bytecode does not match genesis configuration for address ${implAddress}`,
      )
      this.hasDiff = true
      return
    }
  }
}
