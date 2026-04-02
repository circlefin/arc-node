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
import hre from 'hardhat'
import { getChain } from '../../scripts/hardhat/viem-helper'
import { toHex, pad, Address, Hex } from 'viem'

const BLOCK_HASH_HISTORY: Address = '0x0000F90827F1C53a10cb7A02335B175320002935'
const EXPECTED_CODEHASH = '0x6e49e66782037c0555897870e29fa5e552daf4719552131a0abce779daec0a5d'

// EIP-2935 system caller -- only this address can write block hashes to the contract.
const SYSTEM_ADDRESS: Address = '0xffffFFFfFFffffffffffffffFfFFFfffFFFfFFfE'

describe('BlockHashHistory (EIP-2935) simulation', () => {
  const clients = async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    return { client }
  }

  it('contract is deployed with correct codehash', async () => {
    const { client } = await clients()
    const code = await client.getCode({ address: BLOCK_HASH_HISTORY })
    expect(code).to.not.equal('0x')
    expect(code!.length).to.be.greaterThan(2)

    const proofResponse = await client.getProof({ address: BLOCK_HASH_HISTORY, storageKeys: [] })
    expect(proofResponse.codeHash).to.equal(EXPECTED_CODEHASH)
  })

  // Simulate the EIP-2935 system call (set) followed by a user read (get) in a single block.
  // This verifies functional correctness even before Zero5 is activated, because
  // simulateBlocks lets us impersonate the system address.
  it('system caller can set a block hash and user can read it back', async () => {
    const { client } = await clients()
    const blockNumber = await client.getBlockNumber()

    // The system call sends the parent hash as 32-byte calldata.
    // The contract stores it at slot (block.number - 1) % 8191.
    const fakeParentHash: Hex = pad('0xdeadbeef', { size: 32 })

    // Need a future block to pass the simulation call.
    // In the simulated block (blockNumber + 10), the contract will store
    // fakeParentHash at slot (blockNumber + 10 - 1) % 8191.
    // To read it back, we query with blockNumber as calldata.
    const readCalldata = toHex(blockNumber + 9n, { size: 32 })

    const result = await client.simulateBlocks({
      blocks: [
        {
          blockOverrides: { number: blockNumber + 10n },
          calls: [
            // Set: system caller writes the parent hash
            {
              account: SYSTEM_ADDRESS,
              to: BLOCK_HASH_HISTORY,
              data: fakeParentHash,
            },
            // Get: regular read for blockNumber (which maps to the same slot)
            {
              to: BLOCK_HASH_HISTORY,
              data: readCalldata,
            },
          ],
        },
      ],
    })

    const calls = result[0].calls
    expect(calls[0].status).to.eq('success', 'system call (set) should succeed')
    expect(calls[1].status).to.eq('success', 'user call (get) should succeed')
    expect(calls[1].data).to.eq(fakeParentHash, 'read-back hash should match what was written')
  })

  // Verify that a non-system caller cannot write to the contract.
  // When a regular address calls with hash-like data, the contract treats it as a
  // read (get) for that block number, which will fail because the value is out of range.
  it('non-system caller cannot set a block hash', async () => {
    const { client } = await clients()

    const fakeHash: Hex = pad('0xcafebabe', { size: 32 })
    const result = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              to: BLOCK_HASH_HISTORY,
              data: fakeHash,
            },
          ],
        },
      ],
    })

    // The contract interprets this as a read for a huge block number, which reverts
    expect(result[0].calls[0].status).to.eq('failure', 'non-system caller should not succeed')
  })

  // Verify the contract reverts for a far-future block number.
  it('reverts for a future block number', async () => {
    const { client } = await clients()
    const blockNumber = await client.getBlockNumber()
    const futureBlock = blockNumber + 10000n

    const result = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              to: BLOCK_HASH_HISTORY,
              data: toHex(futureBlock, { size: 32 }),
            },
          ],
        },
      ],
    })

    expect(result[0].calls[0].status).to.eq('failure', 'future block query should fail')
  })
})
