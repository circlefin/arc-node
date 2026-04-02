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

import { task, types } from 'hardhat/config'
import fs from 'fs'
import path from 'path'
import localBuilder, { localBuilderOptionsSchema } from '../../../assets/localdev/genesis.config'
import devnetBuilder from '../../../assets/devnet/genesis.config'
import testnetBuilder from '../../../assets/testnet/genesis.config'
import { hardforkNameSchema, initialHardforksByName } from '../../genesis'

type GenesisArgs = {
  numValidators?: number
  numExtraAccounts?: number
  outputDir?: string
  outputSuffix?: string
  overridePublicKeys?: string
  validatorNames?: string
  votingPowers?: string
  hardfork: string
}

task('genesis', 'Generate the genesis file')
  .addOptionalParam('numValidators', 'Number of validators for localdev network', 5, types.int)
  .addOptionalParam('numExtraAccounts', 'Number of extra prefunded accounts that will send transactions', 0, types.int)
  .addOptionalParam('outputDir', 'Path to the output directory', undefined)
  .addOptionalParam('outputSuffix', 'The suffix for output file name', undefined)
  .addOptionalParam(
    'overridePublicKeys',
    'Override validator public keys with these comma-separated values (format: ID:KEY)',
    undefined,
    types.string,
  )
  .addOptionalParam('validatorNames', 'Comma-separated validator names (localdev only)', undefined, types.string)
  .addOptionalParam(
    'votingPowers',
    'Comma-separated voting powers per validator (localdev only)',
    undefined,
    types.string,
  )
  .addOptionalParam('hardfork', 'hardfork to use, available: zero3, zero4, zero5, zero6', 'zero6', types.string)
  .setAction(async (args: GenesisArgs, hre) => {
    const root = hre.config.paths.root
    const net = hre.network.name
    const {
      outputDir = path.join(root, `./assets/${net}`),
      outputSuffix,
      validatorNames,
      votingPowers,
      hardfork,
      ...buildOptions
    } = args
    const outputPathWithSuffix = (name: string) =>
      path.join(outputDir, outputSuffix ? `${name}-${outputSuffix}.json` : `${name}.json`)
    const parsedValidatorNames = validatorNames
      ?.split(',')
      .map((name) => name.trim())
      .filter((name) => name.length > 0)
    const parsedVotingPowers = votingPowers?.split(',').map((s) => {
      const n = Number(s.trim())
      if (isNaN(n)) throw new Error(`Invalid voting power value: '${s.trim()}'`)
      return n
    })
    const hardforkName = hardforkNameSchema.parse(hardfork)

    if (!fs.existsSync(outputDir)) {
      fs.mkdirSync(outputDir, { recursive: true })
    }

    console.log(`Generating genesis for ${net} network`)
    const genesis = await (async () => {
      switch (net) {
        case 'localdev': {
          const options = localBuilderOptionsSchema.parse({
            ...buildOptions,
            ...(parsedValidatorNames && parsedValidatorNames.length > 0
              ? {
                  validatorNames: parsedValidatorNames,
                  outputControllersConfig: outputPathWithSuffix('controllers-config'),
                }
              : {}),
            ...(parsedVotingPowers && parsedVotingPowers.length > 0 ? { votingPowers: parsedVotingPowers } : {}),
            outputGenesisConfig: outputPathWithSuffix('config'),
            hardforks: initialHardforksByName(hardforkName),
          })
          console.log(`Options: ${JSON.stringify(options)}`)
          return localBuilder(options)
        }
        case 'devnet':
          return devnetBuilder()
        case 'testnet':
          return testnetBuilder()
        default:
          throw new Error(`Unsupported network: ${net}`)
      }
    })()

    const output = outputPathWithSuffix('genesis')
    fs.writeFileSync(output, JSON.stringify(genesis, null, 2) + '\n')
    console.log(`Genesis file written to ${output}`)
  })
