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
    // Scenario: localdev validators each advertise a distinct fee recipient, so
    // proposer rotation should route transaction fees to every configured
    // recipient.
    // Call flow: sender → localdev txpool → Malachite proposer rotation → EL
    // block beneficiary.
    // Assertions: all validator recipients are observed as block miners within
    // a bounded block window and accrue fees; fallback and zero-address
    // recipients do not accrue fees.
    it('routes fees to every per-validator recipient as proposer rotates', async function () {
      this.timeout(180_000)

      const { client, sender } = await getClients()

      const [initialPerValidator, initialDefault, initialZero] = await Promise.all([
        Promise.all(LOCALDEV_FEE_RECIPIENTS.map((addr) => client.getBalance({ address: addr }))),
        client.getBalance({ address: LOCALDEV_FEE_RECIPIENT }),
        client.getBalance({ address: zeroAddress }),
      ])

      // Send sequentially until the landed blocks cover every validator
      // recipient, rather than assuming a fixed transaction count samples a
      // uniform proposer rotation. Multiple txs can share a block and round
      // changes can skew short windows, so bound the sample by block span.
      const expectedValidatorCount = LOCALDEV_FEE_RECIPIENTS.length
      const expectedRecipientSet = new Set<Address>(LOCALDEV_FEE_RECIPIENTS)
      const maxBlockSpan = 100n
      const startBlock = await client.getBlockNumber()
      const lastAllowedBlock = startBlock + maxBlockSpan
      const expectedMiners = new Set<Address>()
      const unexpectedMiners = new Set<Address>()
      let txsSent = 0
      let lastSampledBlock = startBlock
      while (expectedMiners.size < expectedValidatorCount && lastSampledBlock < lastAllowedBlock) {
        txsSent += 1
        const hash = await sender.sendTransaction({
          to: sender.account.address,
          value: 1n,
          maxFeePerGas: parseGwei('1000'),
          maxPriorityFeePerGas: parseGwei('10'),
        })
        const receipt = await client.waitForTransactionReceipt({ hash })
        lastSampledBlock = receipt.blockNumber
        const block = await client.getBlock({ blockNumber: receipt.blockNumber })
        if (expectedRecipientSet.has(block.miner)) {
          expectedMiners.add(block.miner)
        } else {
          unexpectedMiners.add(block.miner)
        }
      }

      const [finalPerValidator, finalDefault, finalZero] = await Promise.all([
        Promise.all(LOCALDEV_FEE_RECIPIENTS.map((addr) => client.getBalance({ address: addr }))),
        client.getBalance({ address: LOCALDEV_FEE_RECIPIENT }),
        client.getBalance({ address: zeroAddress }),
      ])

      const deltas = finalPerValidator.map((final, i) => final - initialPerValidator[i])
      const accrued = deltas.filter((d) => d > 0n).length
      const deltaSummary = deltas.map((d, i) => `recipient${i + 1}=${d}`).join(', ')
      const defaultDelta = finalDefault - initialDefault
      const zeroDelta = finalZero - initialZero

      const observedBlockSpan = lastSampledBlock - startBlock
      const expectedMinerSummary = [...expectedMiners].join(', ')
      const unexpectedMinerSummary = [...unexpectedMiners].join(', ')

      expect(unexpectedMiners.size).to.equal(
        0,
        [
          `Unexpected miners observed: ${unexpectedMinerSummary}.`,
          `Expected only LOCALDEV_FEE_RECIPIENTS: ${[...expectedRecipientSet].join(', ')}.`,
        ].join(' '),
      )

      expect(expectedMiners.size).to.equal(
        expectedValidatorCount,
        [
          `Expected all ${expectedValidatorCount} proposers to produce blocks we landed in`,
          `within ${maxBlockSpan} blocks;`,
          `observed ${expectedMiners.size} expected (${expectedMinerSummary}) after ${txsSent} txs over a ${observedBlockSpan}-block span.`,
        ].join(' '),
      )

      expect(accrued).to.equal(
        expectedValidatorCount,
        [
          `Expected all ${expectedValidatorCount} per-validator recipients to accrue fees;`,
          `${accrued} did. ${deltaSummary}.`,
        ].join(' '),
      )

      // Negative controls: fees must not leak to the single-recipient fallback
      // or to the zero address. A mis-configured validator (missing
      // cl_suggested_fee_recipient) would fall back to LOCALDEV_FEE_RECIPIENT.
      expect(defaultDelta).to.equal(
        0n,
        `LOCALDEV_FEE_RECIPIENT received ${defaultDelta} wei; should be zero under per-validator routing.`,
      )
      expect(zeroDelta).to.equal(0n, `zeroAddress received ${zeroDelta} wei; should be zero.`)
    })
  },
)
