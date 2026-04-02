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
import { buildNativeFiatTokenGenesisAllocs, FIAT_TOKEN_ADDRESS, schemaNativeFiatToken } from './NativeFiatToken'
import {
  addressToBytes32,
  buildAccountAlloc,
  buildImplContractAlloc,
  GenesisAccountAlloc,
  schemaAddress,
  schemaBigInt,
  slotForAddressMap,
  slotIndex,
  StorageSlot,
  toBytes32,
} from './types'
import { buildDenylistGenesisAllocs, schemaDenylist } from './Denylist'
import { buildProtocolConfigGenesisAllocs, schemaProtocolConfig } from './ProtocolConfig'
import { BuilderContext } from './context'
import { Address, fromHex, Hex, toHex } from 'viem'
import { buildValidatorManagerGenesisAllocs, schemaValidatorManager } from './ValidatorManager'
import {
  memoAddress,
  multicall3FromAddress,
  nativeCoinAutorityAddress,
  nativeCoinControlAddress,
} from './addresses'

const emptyPrecompileStart = 0x1800000000000000000000000000000000000002n
const emptyPrecompileEnd = 0x18000000000000000000000000000000000000ffn

export const externalContracts = ['DeterministicDeploymentProxy', 'Multicall3', 'BlockHashHistory', 'Permit2'] as const

export const schemaGenesisConfig = z
  .object({
    /**
     * The timestamp of the genesis block. Default to the current time.
     */
    timestamp: schemaBigInt,

    /**
     * The prefund accounts. The balance of the account will be initialized to the specified value in the genesis block.
     */
    prefund: z.array(z.object({ address: schemaAddress, balance: schemaBigInt }).strict()).optional(),

    /**
     * The configuration of the NativeFiatToken contract.
     */
    NativeFiatToken: schemaNativeFiatToken,

    /**
     * The configuration of the ProtocolConfig contract.
     */
    ProtocolConfig: schemaProtocolConfig,

    /**
     * The configuration of the ValidatorManager contracts.
     */
    ValidatorManager: schemaValidatorManager,

    /**
     * The configuration of the Denylist contract (optional; omit for testnet if deployed manually).
     */
    Denylist: schemaDenylist.optional(),

    /**
     * The configuration of the hardforks.
     *
     * For named networks (e.g. start the node by `--chain=arc-localdev`),
     * the hardfork source of truth is the hardfork.rs.
     *
     * This setting is only for internal testing by `--chain=genesis.json`.
     */
    hardforks: z
      .object({
        zero3Block: z.number().optional(),
        zero4Block: z.number().optional(),
        zero5Block: z.number().optional(),
        zero6Block: z.number().optional(),
        osakaTime: z.number().optional(),
      })
      .optional(),

    /**
     * Whether to include the GasGuzzler test contract in genesis.
     */
    GasGuzzler: z.boolean().optional(),

    /**
     * Whether to include the Memo contract in genesis.
     */
    Memo: z.boolean().optional(),

    /**
     * Whether to include the Multicall3From contract in genesis.
     */
    Multicall3From: z.boolean().optional(),

    /**
     * Whether to include the TestToken ERC-20 contract in genesis.
     * When enabled, all prefund accounts receive TestToken balances.
     */
    TestToken: z.boolean().optional(),

    /**
     * The configuration of the external contracts.
     */
    ...Object.fromEntries(externalContracts.map((contractName) => [contractName, z.boolean().optional()])),
  })
  .strict()

export type GenesisConfig = z.infer<typeof schemaGenesisConfig>

// Defines hardfork name, this is used for genesis builder command line arguments.
export const hardforkNameSchema = z.enum(['zero3', 'zero4', 'zero5', 'zero6'])

// Defines the mapping from hardfork name to genesis hardforks initialize setting.
export function initialHardforksByName(hardforkName: z.infer<typeof hardforkNameSchema>): GenesisConfig['hardforks'] {
  return {
    zero3: { zero3Block: 0 },
    zero4: { zero3Block: 0, zero4Block: 0 },
    zero5: { zero3Block: 0, zero4Block: 0, zero5Block: 0, osakaTime: 0 },
    zero6: { zero3Block: 0, zero4Block: 0, zero5Block: 0, zero6Block: 0, osakaTime: 0 },
  }[hardforkName]
}

export const buildGenesis = async (ctx: BuilderContext, config: GenesisConfig) => {
  const parsed = schemaGenesisConfig.parse(config)
  const {
    timestamp,
    prefund,
    NativeFiatToken: nativeFiatToken,
    ProtocolConfig: protocolConfig,
    ValidatorManager: validatorManager,
    Denylist: denylistConfig,
    hardforks,
    GasGuzzler: gasGuzzlerEnabled,
    Memo: memoEnabled,
    Multicall3From: multicall3FromEnabled,
    TestToken: testTokenEnabled,
    ...externalContractsConfig
  } = parsed

  const allocs: Record<Address, GenesisAccountAlloc> = {}
  const insert = ([account, alloc]: [string, GenesisAccountAlloc]) => {
    if (account in allocs) {
      throw new Error(`Duplicate account: ${account}`)
    }
    allocs[schemaAddress.parse(account)] = alloc
  }

  Object.entries(await buildNativeFiatTokenGenesisAllocs(ctx, nativeFiatToken)).forEach(insert)
  Object.entries(await buildProtocolConfigGenesisAllocs(ctx, protocolConfig)).forEach(insert)
  Object.entries(await buildValidatorManagerGenesisAllocs(ctx, validatorManager)).forEach(insert)

  if (denylistConfig != null) {
    Object.entries(await buildDenylistGenesisAllocs(ctx, denylistConfig)).forEach(insert)
  }

  // Add external contracts.
  for (const contractName of externalContracts) {
    const isEnabled = externalContractsConfig[contractName as keyof typeof externalContractsConfig]
    if (isEnabled == null || isEnabled === true) {
      const [address, alloc] = buildAccountAlloc({
        address: await ctx.contractLoader.getDeterministicAddress(contractName),
        balance: 0n,
        nonce: 1n,
        code: await ctx.contractLoader.getCode(contractName),
      })
      insert([address, alloc])

      // Set deployer nonce to 1 for one-time-address contracts to simulate the deployment tx.
      const entry = ctx.contractLoader.manifest[contractName]
      if (entry?.type === 'one-time-address') {
        insert(buildAccountAlloc({ address: entry.deployer, balance: 0n, nonce: 1n }))
      }
    }
  }

  // Add GasGuzzler test contract if enabled.
  if (gasGuzzlerEnabled === true) {
    const [address, alloc] = buildAccountAlloc({
      address: await ctx.contractLoader.getDeterministicAddress('GasGuzzler'),
      balance: 0n,
      nonce: 1n,
      code: await ctx.contractLoader.getCode('GasGuzzler'),
    })
    insert([address, alloc])
  }

  // Add Memo contract if enabled.
  if (memoEnabled === true) {
    insert(await buildImplContractAlloc(ctx, 'Memo', { address: memoAddress }))
  }

  // Add Multicall3From contract if enabled.
  if (multicall3FromEnabled === true) {
    insert(await buildImplContractAlloc(ctx, 'Multicall3From', { address: multicall3FromAddress }))
  }

  // Add TestToken ERC-20 contract if enabled. Prefund accounts receive token balances.
  if (testTokenEnabled === true) {
    const testTokenAddress = await ctx.contractLoader.getDeterministicAddress('TestToken')
    const testTokenCode = await ctx.contractLoader.getCode('TestToken')
    const prefundAddresses = prefund?.map((p) => p.address) ?? []
    const balancePerAccount = 1_000_000n * 10n ** 18n // 1M tokens
    const totalTokenSupply = balancePerAccount * BigInt(prefundAddresses.length)

    // OpenZeppelin ERC20 storage layout:
    //   slot 0: _balances (mapping(address => uint256))
    //   slot 2: _totalSupply (uint256)
    const storage = [
      StorageSlot(slotIndex(2n), toBytes32(totalTokenSupply)),
      ...prefundAddresses.map((addr) => StorageSlot(slotForAddressMap(0n, addr), toBytes32(balancePerAccount))),
    ]
    const [address, alloc] = buildAccountAlloc({
      address: testTokenAddress,
      balance: 0n,
      nonce: 1n,
      code: testTokenCode,
      storage,
    })
    insert([address, alloc])
  }

  // Fill empty precompiles
  const defaultPrecompile = { balance: 0n, nonce: 1n, code: '0x01' as Hex }
  for (let i = emptyPrecompileStart; i <= emptyPrecompileEnd; i++) {
    insert(buildAccountAlloc({ ...defaultPrecompile, address: toHex(i, { size: 20 }) }))
  }

  // Add prefund accounts.
  prefund?.forEach((prefund) => {
    insert(buildAccountAlloc({ address: prefund.address, balance: prefund.balance, nonce: 0n }))
  })

  // In the last step, setup native mint authority with the total supply.
  const totalSupply = Object.values(allocs)
    .filter((alloc) => alloc.balance != null)
    .reduce((acc, alloc) => acc + fromHex(alloc.balance, 'bigint'), 0n)
  insert(
    buildAccountAlloc({
      ...defaultPrecompile,
      address: nativeCoinAutorityAddress,
      storage: [
        // deprecated since Zero5
        StorageSlot(slotIndex(1n), addressToBytes32(nativeFiatToken.proxy.address ?? FIAT_TOKEN_ADDRESS)),
        StorageSlot(slotIndex(2n), toBytes32(totalSupply)),
      ],
    }),
  )

  // Setup native coin control precompile with FiatToken contract address as caller.
  insert(
    buildAccountAlloc({
      ...defaultPrecompile,
      address: nativeCoinControlAddress,
      storage: [
        // deprecated since Zero5
        StorageSlot(slotIndex(1n), addressToBytes32(nativeFiatToken.proxy.address ?? FIAT_TOKEN_ADDRESS)),
      ],
    }),
  )

  return {
    config: {
      chainId: ctx.chainId,
      daoForkSupport: false,
      terminalTotalDifficulty: '0x0',
      terminalTotalDifficultyPassed: true,
      homesteadBlock: 0,
      eip150Block: 0,
      eip150Hash: '0x0000000000000000000000000000000000000000000000000000000000000000',
      eip155Block: 0,
      eip158Block: 0,
      byzantiumBlock: 0,
      constantinopleBlock: 0,
      petersburgBlock: 0,
      istanbulBlock: 0,
      muirGlacierBlock: 0,
      berlinBlock: 0,
      londonBlock: 0,
      arrowGlacierBlock: 0,
      grayGlacierBlock: 0,
      shanghaiTime: 0,
      cancunTime: 0,
      pragueTime: 0,
      ...hardforks,
    },
    nonce: '0x0',
    timestamp: toHex(timestamp),
    extraData: '0x',
    gasLimit: toHex(protocolConfig.feeParams.blockGasLimit ?? 30_000_000n),
    difficulty: '0x0',
    mixHash: toBytes32(0n),
    coinbase: protocolConfig.beneficiary,
    number: '0x0',
    alloc: allocs,
  }
}
