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

import { parseEther } from 'viem'
import { createBuilderContext, buildGenesis, schemaGenesisConfig, GenesisConfig } from '../../scripts/genesis'
import fs from 'fs'
import path from 'path'
import { bigintReplacer, currentTimestamp } from '../../scripts/genesis/types'
import { MnemonicAccountCreator } from '../../scripts/genesis/AccountCreator'

const build = async () => {
  const ctx = await createBuilderContext({ network: 'testnet', chainId: 5042002 })
  const configPath = path.join(ctx.projectRoot, `assets/${ctx.network}/config.json`)
  const walletSecretsPath = path.join(ctx.projectRoot, `assets/${ctx.network}/wallet-secrets.json`)

  // Load existing config file for CI
  if (fs.existsSync(configPath)) {
    const config = schemaGenesisConfig.parse(JSON.parse(fs.readFileSync(configPath, 'utf8')))
    return await buildGenesis(ctx, config)
  }

  if (!process.env.ARC_TESTNET_ADMIN_MNEMONIC) {
    throw new Error('ARC_TESTNET_ADMIN_MNEMONIC is not set')
  }
  if (!process.env.ARC_TESTNET_VALIDATOR_MNEMONIC) {
    throw new Error('ARC_TESTNET_VALIDATOR_MNEMONIC is not set')
  }
  const creator = new MnemonicAccountCreator({
    adminMnemonic: process.env.ARC_TESTNET_ADMIN_MNEMONIC,
    validatorMnemonic: process.env.ARC_TESTNET_VALIDATOR_MNEMONIC,
  })
  const adminPrefund = parseEther('1000')

  const config: GenesisConfig = {
    timestamp: currentTimestamp(),

    NativeFiatToken: {
      proxy: { admin: creator.nextAccount('FiatTokenCircleChain.proxyAdmin', adminPrefund) },
      owner: creator.nextAccount('FiatTokenCircleChain.owner', adminPrefund),
      pauser: creator.nextAccount('FiatTokenCircleChain.pauser', adminPrefund),
      masterMinter: creator.nextAccount('FiatTokenCircleChain.masterMinter', adminPrefund),
      rescuer: creator.nextAccount('FiatTokenCircleChain.rescuer', adminPrefund),
      blacklister: creator.nextAccount('FiatTokenCircleChain.blacklister', adminPrefund),
      minters: [
        {
          address: creator.nextAccount('FiatTokenCircleChain.minter', adminPrefund),
          allowance: parseEther('1000000'),
        },
      ],
    },

    ProtocolConfig: {
      proxy: { admin: creator.nextAccount('ProtocolConfig.proxyAdmin', adminPrefund) },
      owner: creator.nextAccount('ProtocolConfig.owner', adminPrefund),
      controller: creator.nextAccount('ProtocolConfig.controller', adminPrefund),
      pauser: creator.nextAccount('ProtocolConfig.pauser', adminPrefund),
      beneficiary: creator.nextAccount('ProtocolConfig.beneficiary'),
      feeParams: {
        alpha: 20n,
        kRate: 25n,
        inverseElasticityMultiplier: 5000n,
        minBaseFee: 1n,
        maxBaseFee: 1000n,
        blockGasLimit: 30_000_000n,
      },
    },

    ValidatorManager: {
      proxy: { admin: creator.nextAccount('ValidatorManager.proxyAdmin', adminPrefund) },
      PermissionedValidatorManager: {
        proxy: { admin: creator.nextAccount('PermissionedValidatorManager.proxyAdmin', adminPrefund) },
        owner: creator.nextAccount('PermissionedValidatorManager.owner', adminPrefund),
        validatorRegisterers: [
          creator.nextAccount('PermissionedValidatorManager.validatorRegisterer1', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.validatorRegisterer2', adminPrefund),
        ],
        controllers: [
          creator.nextAccount('PermissionedValidatorManager.controller1', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller2', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller3', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller4', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller5', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller6', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller7', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller8', adminPrefund),
          creator.nextAccount('PermissionedValidatorManager.controller9', adminPrefund),
        ],
      },
      validators: [
        await creator.nextValidatorKey('validator1', 2000n),
        await creator.nextValidatorKey('validator2', 2000n),
        await creator.nextValidatorKey('validator3', 2000n),
      ],
    },
  }

  // Add the prefund accounts, collect from the creator.
  config.prefund = creator.getPrefunds()

  // Output secrets for the first time generation
  fs.writeFileSync(walletSecretsPath, JSON.stringify(creator.getAdminConfig(), null, 2))
  for (const validator of creator.getValidatorConfig()) {
    fs.writeFileSync(
      path.join(`assets/${ctx.network}`, `${validator.name}.json`),
      JSON.stringify(validator, bigintReplacer, 2),
    )
  }

  // Save config to file. Then CI do not required the mnemonic.
  fs.writeFileSync(configPath, JSON.stringify(config, bigintReplacer, 2))

  return await buildGenesis(ctx, config)
}

export default build
