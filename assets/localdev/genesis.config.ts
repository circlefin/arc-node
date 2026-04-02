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
import { z } from 'zod'
import { parseEther, parseGwei, toHex } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { createBuilderContext, buildGenesis, GenesisConfig, schemaGenesisConfig } from '../../scripts/genesis'
import { bigintReplacer } from '../../scripts/genesis/types'
import { LocalDevAccountCreator } from '../../scripts/genesis/AccountCreator'

// localBuilderOptionsSchema defines the options to customize the localdev genesis.
export const localBuilderOptionsSchema = LocalDevAccountCreator.optionsSchema.and(
  z.object({
    outputControllersConfig: z.string().optional(),
    outputGenesisConfig: z.string().optional(),
    validatorNames: z.array(z.string()).min(1).optional(),
    hardforks: schemaGenesisConfig.shape.hardforks,
  }),
)

const build = async (options: z.infer<typeof localBuilderOptionsSchema>) => {
  const ctx = await createBuilderContext({
    network: 'localdev',
    chainId: 1337,
  })
  const { outputControllersConfig, outputGenesisConfig, validatorNames, hardforks, ...accountOptions } = options
  const accountCreator = new LocalDevAccountCreator(accountOptions)

  // Default account for hardhat environment.
  const accounts = accountCreator.defaultAccounts()
  const { operator, admin, proxyAdmin } = accountCreator.namedAccounts(accounts)

  const one = privateKeyToAccount(toHex(1n, { size: 32 }))
  const controllers = accountCreator.controllers()
  const validators = await accountCreator.validators()

  if (outputControllersConfig) {
    if (!validatorNames) {
      throw new Error('validatorNames is required when outputControllersConfig is set')
    }
    if (validatorNames.length !== controllers.length) {
      throw new Error(
        `validatorNames length (${validatorNames.length}) must match controllers length (${controllers.length})`,
      )
    }
    const uniqueNames = new Set(validatorNames)
    if (uniqueNames.size !== validatorNames.length) {
      throw new Error('validatorNames must be unique')
    }
    // Dump controllers config file for quake reference
    const controllersConfig = Object.fromEntries(
      controllers.map((x, i) => {
        const key = validatorNames[i]
        return [key, { index: i + 1, address: x.address, signingKey: toHex(x.getHdKey().privateKey!), nonce: 0 }]
      }),
    )
    fs.writeFileSync(outputControllersConfig, JSON.stringify(controllersConfig, null, 2))
  }

  const config: GenesisConfig = {
    timestamp: 1763620028n,
    hardforks: {
      zero3Block: 0,
      ...hardforks,
    },

    prefund: accounts
      .map((account) => account.address)
      .concat([one.address])
      .concat(controllers.map((x) => x.address))
      .concat(accountCreator.extraPrefundAccounts().map((x) => x.address))
      .map((address) => ({ address: address, balance: parseEther('1000000') })),

    NativeFiatToken: {
      proxy: { admin: proxyAdmin.address },
      owner: admin.address,
      pauser: admin.address,
      masterMinter: admin.address,
      rescuer: admin.address,
      blacklister: operator.address,
      minters: [
        { address: operator.address, allowance: parseEther('1000000') },
        ...accountCreator.extraMinters().map((x) => ({ address: x.address, allowance: parseEther('1000000') })),
      ],
    },

    ProtocolConfig: {
      proxy: { admin: proxyAdmin.address },
      owner: admin.address,
      controller: admin.address,
      pauser: admin.address,
      beneficiary: proxyAdmin.address,
      feeParams: {
        alpha: 20n, // 20%
        kRate: 200n, // 2%
        inverseElasticityMultiplier: 5000n, // 50%
        minBaseFee: 1n,
        maxBaseFee: parseGwei('1000'),
        blockGasLimit: 30_000_000n,
      },
      consensusParams: {
        timeoutProposeMs: 3000n,
        timeoutProposeDeltaMs: 500n,
        timeoutPrevoteMs: 1000n,
        timeoutPrevoteDeltaMs: 500n,
        timeoutPrecommitMs: 1000n,
        timeoutPrecommitDeltaMs: 500n,
        timeoutRebroadcastMs: 1000n,
        targetBlockTimeMs: 500n,
      },
    },

    Denylist: {
      proxy: { admin: proxyAdmin.address },
      owner: admin.address,
      denylisters: [operator.address],
    },

    ValidatorManager: {
      proxy: { admin: proxyAdmin.address },
      PermissionedValidatorManager: {
        proxy: { admin: proxyAdmin.address },
        owner: admin.address,
        validatorRegisterers: [admin.address, operator.address],
        controllers: controllers.map((account) => account.address),
      },
      validators: validators.map((x) => ({
        publicKey: x.publicKey,
        votingPower: x.votingPower,
      })),
    },
    GasGuzzler: true,
    Memo: true,
    Multicall3From: true,
    TestToken: true,
  }

  if (outputGenesisConfig) {
    // Save config to file. Then CI do not required the mnemonic.
    fs.writeFileSync(outputGenesisConfig, JSON.stringify(config, bigintReplacer, 2))
  }

  return await buildGenesis(ctx, config)
}

export default build
