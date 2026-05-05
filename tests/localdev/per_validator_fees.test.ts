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
import { Address, parseGwei, zeroAddress } from 'viem'
import { getClients, LOCALDEV_FEE_RECIPIENT, LOCALDEV_FEE_RECIPIENTS } from '../helpers'

// Only runs under `make smoke-malachite` (ARC_SMOKE_SCENARIO=malachite). Requires
// `localdev.toml` (per-validator recipients); smoke-reth (reth --dev) doesn't
// rotate proposers. The EL uses the validator-supplied beneficiary
// unconditionally, so no ProtocolConfig setup is needed here.
;(process.env.ARC_SMOKE_SCENARIO === 'malachite' ? describe : describe.skip)(
  'per-validator fee accrual (malachite)',
  () => {
    it('routes fees to every per-validator recipient as proposer rotates', async function () {
      this.timeout(180_000)

      const { client, sender } = await getClients()

      const [initialPerValidator, initialDefault, initialZero] = await Promise.all([
        Promise.all(LOCALDEV_FEE_RECIPIENTS.map((addr) => client.getBalance({ address: addr }))),
        client.getBalance({ address: LOCALDEV_FEE_RECIPIENT }),
        client.getBalance({ address: zeroAddress }),
      ])

      // Send sequentially so each tx lands in its own block; collect the proposer
      // of every landed block directly from block.miner.
      const numTxs = 25
      const miners = new Set<Address>()
      for (let i = 0; i < numTxs; i++) {
        const hash = await sender.sendTransaction({
          to: sender.account.address,
          value: 1n,
          maxFeePerGas: parseGwei('1000'),
          maxPriorityFeePerGas: parseGwei('10'),
        })
        const receipt = await client.waitForTransactionReceipt({ hash })
        const block = await client.getBlock({ blockNumber: receipt.blockNumber })
        miners.add(block.miner)
      }

      const [finalPerValidator, finalDefault, finalZero] = await Promise.all([
        Promise.all(LOCALDEV_FEE_RECIPIENTS.map((addr) => client.getBalance({ address: addr }))),
        client.getBalance({ address: LOCALDEV_FEE_RECIPIENT }),
        client.getBalance({ address: zeroAddress }),
      ])

      const deltas = finalPerValidator.map((final, i) => final - initialPerValidator[i])
      const accrued = deltas.filter((d) => d > 0n).length
      const deltaSummary = deltas.map((d, i) => `recipient${i + 1}=${d}`).join(', ')

      // Every one of our txs landed in a block proposed by one of the 5 validators;
      // observing all 5 proves rotation covered our sample (not just the chain's
      // history).
      expect(miners.size).to.equal(
        5,
        `Expected all 5 proposers to produce blocks we landed in; observed ${miners.size} (${[...miners].join(', ')}).`,
      )

      expect(accrued).to.equal(
        5,
        `Expected all 5 per-validator recipients to accrue fees; ${accrued} did. ${deltaSummary}.`,
      )

      // Negative controls: fees must not leak to the single-recipient fallback
      // or to the zero address. A mis-configured validator (missing
      // cl_suggested_fee_recipient) would fall back to LOCALDEV_FEE_RECIPIENT.
      expect(finalDefault - initialDefault).to.equal(
        0n,
        `LOCALDEV_FEE_RECIPIENT received ${finalDefault - initialDefault} wei; should be zero under per-validator routing.`,
      )
      expect(finalZero - initialZero).to.equal(
        0n,
        `zeroAddress received ${finalZero - initialZero} wei; should be zero.`,
      )
    })
  },
)
