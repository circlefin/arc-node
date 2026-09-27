// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

import { strict as assert } from 'node:assert'
import { Address, fromHex, getAddress, toHex } from 'viem'
import { nativeCoinAutorityAddress } from '../../scripts/genesis/addresses'
import { BuilderContext, ContractLoader } from '../../scripts/genesis/context'
import { buildGenesis, externalContracts, GenesisConfig } from '../../scripts/genesis/genesis'
import { slotIndex } from '../../scripts/genesis/types'

const admin = '0x1111111111111111111111111111111111111111'
const operator = '0x2222222222222222222222222222222222222222'
const account = '0xabcdefabcdefabcdefabcdefabcdefabcdefabcd'
const checksumAccount = getAddress(account)
const otherAccount = '0xabcdefabcdefabcdefabcdefabcdefabcdefabce'
const implementationAddress = '0xabcdef0000000000000000000000000000000001'

function context(): BuilderContext {
  return {
    network: 'localdev',
    chainId: 1337,
    projectRoot: process.cwd(),
    // Only artifact loading is stubbed; all genesis allocation builders run normally.
    contractLoader: new ContractLoader(
      {
        SignatureChecker: { address: toHex(0x1001n, { size: 20 }), code: '0x00' },
        NativeFiatTokenV2_2: { address: implementationAddress, code: '0x00' },
        FiatTokenProxy: { address: toHex(0x1003n, { size: 20 }), code: '0x00' },
        ProtocolConfig: { address: toHex(0x1004n, { size: 20 }), code: '0x00' },
        ValidatorRegistry: { address: toHex(0x1005n, { size: 20 }), code: '0x00' },
        PermissionedValidatorManager: { address: toHex(0x1006n, { size: 20 }), code: '0x00' },
        AdminUpgradeableProxy: { address: toHex(0x1007n, { size: 20 }), code: '0x00' },
      },
      {},
    ),
  }
}

function config(prefund: Array<{ address: Address; balance: bigint }>): GenesisConfig {
  return {
    timestamp: 0n,
    coinbase: operator,
    prefund,
    NativeFiatToken: {
      proxy: { admin },
      owner: operator,
      pauser: operator,
      blacklister: operator,
      masterMinter: operator,
      rescuer: operator,
      minters: [],
    },
    ProtocolConfig: {
      proxy: { admin },
      owner: operator,
      controller: operator,
      pauser: operator,
      feeParams: {
        alpha: 1n,
        kRate: 1n,
        inverseElasticityMultiplier: 1n,
        minBaseFee: 0n,
        maxBaseFee: 1_000_000n,
        blockGasLimit: 30_000_000n,
      },
    },
    ValidatorManager: {
      proxy: { admin },
      validators: [
        {
          publicKey: `0x${'11'.repeat(32)}`,
          votingPower: 20n,
          controllers: [{ address: operator, votingPowerLimit: 100n }],
        },
      ],
      PermissionedValidatorManager: {
        proxy: { admin },
        owner: operator,
        pauser: operator,
        validatorRegisterers: [operator],
      },
    },
    ...Object.fromEntries(externalContracts.map((name) => [name, false])),
  }
}

describe('genesis account collisions', () => {
  for (const addresses of [
    [account, checksumAccount],
    [checksumAccount, account],
    [account, account],
  ] as const) {
    it(`rejects duplicate prefunds ${addresses.join(' / ')}`, async () => {
      await assert.rejects(buildGenesis(context(), config(addresses.map((address) => ({ address, balance: 10n })))), {
        message: `Duplicate account: ${addresses[1]}`,
      })
    })
  }

  it('rejects a prefund alias of a contract implementation', async () => {
    const address = getAddress(implementationAddress)
    assert.notEqual(address, implementationAddress)
    await assert.rejects(buildGenesis(context(), config([{ address, balance: 10n }])), {
      message: `Duplicate account: ${address}`,
    })
  })

  it('rejects a prefund alias of a reserved precompile', async () => {
    const address = '0x180000000000000000000000000000000000000A'
    await assert.rejects(buildGenesis(context(), config([{ address, balance: 10n }])), {
      message: `Duplicate account: ${address}`,
    })
  })

  it('preserves distinct accounts, their casing, contract code, and total supply', async () => {
    const genesis = await buildGenesis(
      context(),
      config([
        { address: checksumAccount, balance: 10n },
        { address: otherAccount, balance: 20n },
      ]),
    )

    assert.equal(genesis.alloc[checksumAccount].balance, '0xa')
    assert.equal(genesis.alloc[otherAccount].balance, '0x14')
    assert.equal(genesis.alloc[account], undefined)
    assert.equal(genesis.alloc[implementationAddress].code, '0x00')
    assert.equal(genesis.alloc[implementationAddress].nonce, '0x1')
    const supply = genesis.alloc[nativeCoinAutorityAddress].storage?.[slotIndex(2n)]
    assert.ok(supply)
    assert.equal(fromHex(supply, 'bigint'), 30n)
    const addresses = Object.keys(genesis.alloc).map((address) => address.toLowerCase())
    assert.equal(new Set(addresses).size, addresses.length)
  })
})
