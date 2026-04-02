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

import * as ed from '@noble/ed25519'
import { Address, Hex, toHex } from 'viem'
import { mnemonicToAccount } from 'viem/accounts'
import { z } from 'zod'
import arcRemoteSignerKeys from '../../tests/helpers/arc-remote-signer-keys.json'

/**
 * Get override public keys based on environment:
 * - If LOAD_PREDEFINED_ARC_REMOTE_SIGNER_KEYS=true, use arc-remote-signer keys from JSON
 * - Otherwise, use empty string (mnemonic-derived keys)
 */
function defaultOverridePublicKeys(): string {
  if (process.env.LOAD_PREDEFINED_ARC_REMOTE_SIGNER_KEYS === 'true') {
    return arcRemoteSignerKeys.keys.map((key, i) => `${i + 1}:${key}`).join(',')
  }
  return ''
}

export type AdminAccount = {
  name: string
  address: Address
  privateKey: Hex
  bip44path?: string
}

export type ValidatorAccount = {
  name: string
  type: 'tendermint/PrivKeyEd25519'
  value: string // private key encoded by base64
  publicKey: Hex
  initialVotingPower: bigint
  bip44path?: string
}

export class MnemonicAccountCreator {
  private readonly adminMnemonic: string
  private readonly validatorMnemonic: string
  private readonly admins: Array<AdminAccount>
  private readonly roleNames: Set<string>
  private readonly validators: Array<ValidatorAccount>
  private readonly prefund: Record<Address, bigint>

  constructor({ adminMnemonic, validatorMnemonic }: { adminMnemonic: string; validatorMnemonic: string }) {
    this.adminMnemonic = adminMnemonic
    this.validatorMnemonic = validatorMnemonic
    this.admins = []
    this.validators = []
    this.prefund = {}
    this.roleNames = new Set()
  }

  private registerRole = (name: string) => {
    if (this.roleNames.has(name)) {
      throw new Error(`Duplicate role name: ${name}`)
    }
    this.roleNames.add(name)
  }

  nextAccount = (name: string, prefund: bigint = 0n) => {
    this.registerRole(name)

    const bip44path = `m/44'/60'/0'/0/${this.admins.length}` as const
    const account = mnemonicToAccount(this.adminMnemonic, { path: bip44path })
    const privateKey = account.getHdKey().privateKey
    if (privateKey == null) {
      throw new Error('private key is null')
    }
    this.admins.push({ name, address: account.address, privateKey: toHex(privateKey), bip44path })
    if (prefund > 0n) {
      if (account.address in this.prefund) {
        throw new Error(`Duplicate prefund account: ${account.address}`)
      }
      this.prefund[account.address] = prefund
    }
    return account.address
  }

  nextValidatorKey = async (name: string, votingPower: bigint) => {
    this.registerRole(name)

    const bip44path = `m/44'/60'/0'/0/${this.validators.length}` as const
    const account = mnemonicToAccount(this.validatorMnemonic, { path: bip44path })
    const privateKey = account.getHdKey().privateKey
    if (privateKey == null) {
      throw new Error('private key is null')
    }
    const publicKey = toHex(await ed.getPublicKeyAsync(privateKey))
    this.validators.push({
      name,
      type: 'tendermint/PrivKeyEd25519',
      value: Buffer.from(privateKey).toString('base64'),
      publicKey,
      initialVotingPower: votingPower,
      bip44path,
    })
    return { publicKey, votingPower }
  }

  getPrefunds = () =>
    Object.entries(this.prefund).map(([address, balance]) => ({ address: address as Address, balance }))
  getAdminConfig = () => this.admins as readonly AdminAccount[]
  getValidatorConfig = () => this.validators as readonly ValidatorAccount[]
}

export class LocalDevAccountCreator {
  static optionsSchema = z.object({
    // Number of validators, default is 5.
    numValidators: z.number().min(1).optional(),
    // number of extra prefunded accounts, default is 0.
    numExtraAccounts: z.number().min(0).optional(),
    // number of extra minters, default is 0.
    numExtraMinters: z.number().min(0).optional(),
    // number of controllers, default is numValidators.
    numControllers: z.number().min(1).optional(),
    // override public keys for validators, format: ID:KEY,ID:KEY
    overridePublicKeys: z.string().optional(),
    // per-validator voting powers; when omitted every validator gets the default (20)
    votingPowers: z.array(z.number().min(1)).optional(),
  })

  numValidators: number
  numExtraAccounts: number
  numExtraMinters: number
  numControllers: number
  overridePublicKeys: Map<number, Hex>
  votingPowers?: number[]
  mnemonic: string

  constructor(options?: z.infer<typeof LocalDevAccountCreator.optionsSchema>) {
    const {
      numValidators = 5,
      numExtraAccounts = 0,
      numExtraMinters = 0,
      numControllers = numValidators,
      overridePublicKeys = defaultOverridePublicKeys(),
      votingPowers,
    } = options ? LocalDevAccountCreator.optionsSchema.parse(options) : {}
    this.numValidators = numValidators
    this.numExtraAccounts = numExtraAccounts
    this.numExtraMinters = numExtraMinters
    this.numControllers = numControllers
    this.overridePublicKeys = parseOverridePublicKeys(overridePublicKeys)
    if (votingPowers && votingPowers.length !== numValidators) {
      throw new Error(`votingPowers length (${votingPowers.length}) must match numValidators (${numValidators})`)
    }
    this.votingPowers = votingPowers
    this.mnemonic = 'test test test test test test test test test test test junk'
  }

  // Default accounts, BIP44: m/44'/60'/0'/0/{0~9}
  defaultAccounts = () =>
    Array.from({ length: 10 }).map((_, i) => mnemonicToAccount(this.mnemonic, { path: `m/44'/60'/0'/0/${i}` }))

  namedAccounts = <T>(accounts: T[]) => {
    const [sender, receiver] = accounts
    const [operator, admin, proxyAdmin] = accounts.slice(7, 10)
    return { sender, receiver, operator, admin, proxyAdmin }
  }

  // Extra prefunded accounts for load testing, BIP44: m/44'/60'/1'/0/{index}
  extraPrefundAccounts = () =>
    Array.from({ length: this.numExtraAccounts }).map((_, i) =>
      mnemonicToAccount(this.mnemonic, { path: `m/44'/60'/1'/0/${i}` }),
    )

  // Extra minters for load testing, share the same BIP44 path with extraPrefundAccounts.
  extraMinters = () =>
    Array.from({ length: this.numExtraMinters }).map((_, i) =>
      mnemonicToAccount(this.mnemonic, { path: `m/44'/60'/1'/0/${i}` }),
    )

  // Controller accounts, BIP44: m/44'/60'/2'/0/{index}
  controllers = () =>
    Array.from({ length: this.numControllers }).map((_, i) =>
      mnemonicToAccount(this.mnemonic, { path: `m/44'/60'/2'/0/${i}` }),
    )

  // Get the controller account by registrationID, BIP44: m/44'/60'/2'/0/{registrationID - 1}
  getController = (registrationID: bigint, existing = true) => {
    if (registrationID < 1n) {
      throw new Error(`Invalid registrationID: ${registrationID}`)
    }
    if (existing) {
      // verify the controller is in the genesis
      if (registrationID > BigInt(this.numControllers)) {
        throw new Error(
          `the controller (registrationID: ${registrationID}) is not in the genesis (${this.numControllers} controllers)`,
        )
      }
    }
    const index = Number(registrationID) - 1
    return mnemonicToAccount(this.mnemonic, { path: `m/44'/60'/2'/0/${index}` })
  }

  // ED25519 public/private key for validators, BIP44: m/44'/60'/0'/1/{2 + index}
  // The start offset is 2 for backward compatibility to previous version of genesis builder.
  validators = async () =>
    await Promise.all(
      Array.from({ length: this.numValidators }).map(async (_, i) => {
        const registrationID = i + 1

        const publicKey: Hex = await (async () => {
          if (this.overridePublicKeys.has(registrationID)) {
            return this.overridePublicKeys.get(registrationID)!
          } else {
            const path = `m/44'/60'/0'/1/${i + 2}` as const
            const privateKey = mnemonicToAccount(this.mnemonic, { path }).getHdKey().privateKey
            if (privateKey == null) {
              throw new Error('private key is null')
            }
            return toHex(await ed.getPublicKeyAsync(privateKey))
          }
        })()

        return {
          registrationID: BigInt(registrationID),
          votingPower: BigInt(this.votingPowers?.[i] ?? 20),
          publicKey,
        }
      }),
    )
}

const ED25519_PUBLIC_KEY_HEX_LENGTH = 66 // 0x prefix + 64 hex chars (32 bytes)

function parseOverridePublicKeys(input: string): Map<number, Hex> {
  const map = new Map<number, Hex>()
  if (input.trim() === '') {
    return map
  }

  const entries = input.split(',')

  for (const entry of entries) {
    const [idStr, key, ...extra] = entry.split(':')

    if (extra.length > 0) {
      throw new Error(`Invalid format in overridePublicKeys: ${entry} (too many colons)`)
    }
    if (!idStr || !key) {
      throw new Error(`Invalid format in overridePublicKeys: ${entry} (missing ID or key)`)
    }

    const id = Number(idStr)

    if (isNaN(id) || id < 1) {
      throw new Error(`Invalid validator ID in overridePublicKeys: ${idStr}`)
    }
    if (map.has(id)) {
      throw new Error(`Duplicate validator ID in overridePublicKeys: ${id}`)
    }
    if (!key.startsWith('0x') || key.length !== ED25519_PUBLIC_KEY_HEX_LENGTH) {
      throw new Error(`Invalid public key format for validator ${id}: ${key}`)
    }

    map.set(id, key as Hex)
  }

  return map
}
