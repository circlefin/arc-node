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

import { expect } from 'chai'
import { Address, fromHex, Hex } from 'viem'
import { Manifest } from '../../scripts/genesis'
import { buildAccountAlloc, GenesisAccountAlloc } from '../../scripts/genesis/types'
import { externalContracts } from '../../scripts/genesis/genesis'
import manifest from '../../assets/artifacts/manifest.json'

const typedManifest = manifest as unknown as Manifest

describe('deployer nonce in genesis', () => {
  describe('manifest structure', () => {
    it('has exactly three one-time-address entries', () => {
      const oneTimeEntries = Object.entries(typedManifest).filter(([, e]) => e.type === 'one-time-address')
      expect(oneTimeEntries).to.have.lengthOf(3)
    })

    it('has at least one deterministic entry', () => {
      const deterministicEntries = Object.entries(typedManifest).filter(([, e]) => e.type === 'deterministic')
      expect(deterministicEntries.length).to.be.greaterThan(0)
    })

    it('one-time-address entries have deployer field', () => {
      for (const [name, entry] of Object.entries(typedManifest)) {
        if (entry.type === 'one-time-address') {
          expect(entry.deployer, `${name} should have deployer`).to.match(/^0x[0-9a-fA-F]{40}$/)
        }
      }
    })

    it('deterministic entries do not have deployer field', () => {
      for (const [name, entry] of Object.entries(typedManifest)) {
        if (entry.type === 'deterministic') {
          expect('deployer' in entry, `${name} should not have deployer`).to.be.false
        }
      }
    })

    it('all external contracts from genesis are present in manifest', () => {
      for (const contractName of externalContracts) {
        expect(typedManifest[contractName], `${contractName} should exist in manifest`).to.not.be.undefined
      }
    })
  })

  describe('deployer alloc construction', () => {
    it('builds deployer alloc with nonce=1 and balance=0', () => {
      const deployerAddress = '0x3fab184622dc19b6109349b94811493bf2a45362' as Address
      const [address, alloc] = buildAccountAlloc({ address: deployerAddress, balance: 0n, nonce: 1n })

      expect(address.toLowerCase()).to.equal(deployerAddress.toLowerCase())
      expect(fromHex(alloc.balance, 'bigint')).to.equal(0n)
      expect(alloc.nonce).to.not.be.undefined
      expect(fromHex(alloc.nonce as Hex, 'bigint')).to.equal(1n)
      expect(alloc.code).to.be.undefined
    })

    it('nonce=0 is omitted from alloc (default behavior)', () => {
      const [, alloc] = buildAccountAlloc({
        address: '0x0000000000000000000000000000000000000001' as Address,
        balance: 0n,
        nonce: 0n,
      })
      expect(alloc.nonce).to.be.undefined
    })

    it('builds correct deployer alloc for each one-time-address entry', () => {
      for (const [name, entry] of Object.entries(typedManifest)) {
        if (entry.type !== 'one-time-address') continue

        const [address, alloc] = buildAccountAlloc({ address: entry.deployer, balance: 0n, nonce: 1n })
        expect(address.toLowerCase(), `${name} deployer address`).to.equal(entry.deployer.toLowerCase())
        expect(fromHex(alloc.balance, 'bigint'), `${name} deployer balance`).to.equal(0n)
        expect(fromHex(alloc.nonce as Hex, 'bigint'), `${name} deployer nonce`).to.equal(1n)
      }
    })
  })

  describe('deployer insertion logic', () => {
    // Simulate the deployer insertion logic from buildGenesis without the full pipeline.
    const simulateDeployerInsertion = (contractManifest: Manifest, enabledContracts: readonly string[]) => {
      const allocs: Record<string, GenesisAccountAlloc> = {}

      for (const contractName of enabledContracts) {
        const entry = contractManifest[contractName]
        if (entry?.type === 'one-time-address') {
          const [addr, alloc] = buildAccountAlloc({ address: entry.deployer, balance: 0n, nonce: 1n })
          if (addr in allocs) {
            throw new Error(`Duplicate deployer account: ${addr}`)
          }
          allocs[addr] = alloc
        }
      }
      return allocs
    }

    it('inserts deployer allocs for all one-time-address contracts', () => {
      const allocs = simulateDeployerInsertion(typedManifest, externalContracts)
      const allocAddresses = Object.keys(allocs).map((a) => a.toLowerCase())

      for (const [name, entry] of Object.entries(typedManifest)) {
        if (entry.type !== 'one-time-address') continue
        expect(allocAddresses, `should include deployer for ${name}`).to.include(entry.deployer.toLowerCase())
      }
    })

    it('does not insert deployer allocs for deterministic contracts', () => {
      const allocs = simulateDeployerInsertion(typedManifest, externalContracts)
      const allocAddresses = Object.keys(allocs).map((a) => a.toLowerCase())

      for (const [name, entry] of Object.entries(typedManifest)) {
        if (entry.type !== 'deterministic') continue
        expect(allocAddresses, `should not include address for deterministic ${name}`).to.not.include(
          entry.address.toLowerCase(),
        )
      }
    })

    it('produces exactly three deployer allocs from the real manifest', () => {
      const allocs = simulateDeployerInsertion(typedManifest, externalContracts)
      expect(Object.keys(allocs)).to.have.lengthOf(3)
    })

    it('skips disabled external contracts', () => {
      const allocs = simulateDeployerInsertion(typedManifest, ['Permit2'] as unknown as readonly string[])
      expect(Object.keys(allocs)).to.have.lengthOf(0)
    })

    it('handles manifest with only deterministic entries', () => {
      const deterministicOnlyManifest: Manifest = {
        TestContract: {
          type: 'deterministic',
          address: '0x000000000022D473030F116dDEE9F6B43aC78BA3',
          salt: '0x0000000000000000000000000000000000000000000000000000000000000001' as Hex,
          ethCodeHash: '0x0000000000000000000000000000000000000000000000000000000000000002' as Hex,
          bytecode: { file: 'test.json', selector: '.bytecode' },
        },
      }
      const allocs = simulateDeployerInsertion(deterministicOnlyManifest, ['TestContract'])
      expect(Object.keys(allocs)).to.have.lengthOf(0)
    })

    it('rejects duplicate deployer addresses', () => {
      const duplicateManifest: Manifest = {
        Contract1: {
          type: 'one-time-address',
          address: '0x4e59b44847b379578588920ca78fbf26c0b4956c',
          deployer: '0x3fab184622dc19b6109349b94811493bf2a45362',
          deployerBalance: '10000000000000000',
          rawTransaction: '0xaa' as Hex,
          ethCodeHash: '0x0000000000000000000000000000000000000000000000000000000000000001' as Hex,
        },
        Contract2: {
          type: 'one-time-address',
          address: '0xcA11bde05977b3631167028862bE2a173976CA11',
          deployer: '0x3fab184622dc19b6109349b94811493bf2a45362',
          deployerBalance: '10000000000000000',
          rawTransaction: '0xbb' as Hex,
          ethCodeHash: '0x0000000000000000000000000000000000000000000000000000000000000002' as Hex,
        },
      }
      const contracts = ['Contract1', 'Contract2'] as unknown as readonly string[]
      expect(() => simulateDeployerInsertion(duplicateManifest, contracts)).to.throw('Duplicate deployer account')
    })
  })
})
