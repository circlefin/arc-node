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
import path from 'path'
import { z } from 'zod'
import { schemaAddress, schemaHex } from './types'
import { spawnSync } from 'child_process'
import { validatorRegistryAddress } from './addresses'

const projectRoot = path.join(__dirname, '../../')
const cacheDir = path.join(projectRoot, 'contracts/cache/')

const schemaContractCode = z.object({ address: schemaAddress, code: schemaHex })
const schemaContractRepo = z.record(z.string(), schemaContractCode)
type ContractRepo = z.infer<typeof schemaContractRepo>

const schemaOneTimeAddressEntry = z.object({
  type: z.literal('one-time-address'),
  address: schemaAddress,
  deployer: schemaAddress,
  deployerBalance: z.string(),
  rawTransaction: schemaHex,
  ethCodeHash: schemaHex,
})

const schemaDeterministicEntry = z.object({
  type: z.literal('deterministic'),
  address: schemaAddress,
  salt: schemaHex,
  ethCodeHash: schemaHex,
  bytecode: z.object({ file: z.string(), selector: z.string() }),
})

const schemaManifestEntry = z.discriminatedUnion('type', [schemaOneTimeAddressEntry, schemaDeterministicEntry])
const schemaManifest = z.record(z.string(), schemaManifestEntry)
export type Manifest = z.infer<typeof schemaManifest>
export type ManifestEntry = z.infer<typeof schemaManifestEntry>

export const schemaNetwork = z.enum(['localdev', 'devnet', 'testnet', 'mainnet'])
export type Network = z.infer<typeof schemaNetwork>

export class ContractLoader {
  static load(_network: Network, chainId: number) {
    // call forge script to get the contract code
    fs.mkdirSync(cacheDir, { recursive: true })
    const outputPath = path.join(cacheDir, `storage-code.${chainId}.json`)
    console.log(`Write storage code to ${outputPath}...`)
    const result = spawnSync(
      'forge',
      [
        'script',
        './contracts/scripts/ArtifactHelper.s.sol',
        '--sig',
        'run(uint256,string,address)',
        `${chainId}`,
        outputPath,
        validatorRegistryAddress,
      ],
      { cwd: projectRoot, stdio: 'inherit' },
    )
    if (result.status !== 0) {
      throw new Error(`Failed to generate storage code`)
    }
    const data = schemaContractRepo.parse(JSON.parse(fs.readFileSync(outputPath, 'utf-8')))
    const manifest = schemaManifest.parse(
      JSON.parse(fs.readFileSync(path.join(projectRoot, 'assets/artifacts/manifest.json'), 'utf-8')),
    )
    return new ContractLoader(data, manifest)
  }

  constructor(
    public repo: ContractRepo,
    public manifest: Manifest,
  ) {}

  getCode = async (name: string) => {
    try {
      return schemaHex.parse(this.repo[name]?.code)
    } catch (e) {
      /* eslint-disable-next-line @typescript-eslint/restrict-template-expressions */
      throw new Error(`Failed to parse contract code for ${name}: ${e}`)
    }
  }

  getDeterministicAddress = async (name: string) => {
    try {
      return schemaAddress.parse(this.repo[name]?.address)
    } catch (e) {
      /* eslint-disable-next-line @typescript-eslint/restrict-template-expressions */
      throw new Error(`Failed to parse contract address for ${name}: ${e}`)
    }
  }
}

export const createBuilderContext = async ({ network, chainId }: { network: Network; chainId: number }) => {
  return {
    network: schemaNetwork.parse(network),
    chainId,
    projectRoot,
    contractLoader: ContractLoader.load(network, chainId),
  }
}

export type BuilderContext = Awaited<ReturnType<typeof createBuilderContext>>
