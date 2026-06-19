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
import { Address, Hash, parseGwei, zeroAddress } from 'viem'
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

      // Keep the mempool fed while proposers rotate. Waiting for each receipt
      // before submitting the next tx can leave empty proposer slots between
      // sends, so submit paced batches and aggregate fees by landed block.
      const expectedValidatorCount = LOCALDEV_FEE_RECIPIENTS.length
      const expectedRecipientSet = new Set<Address>(LOCALDEV_FEE_RECIPIENTS)
      const maxBlockSpan = 100n
      const txsPerBatch = 25
      const txIntervalMs = 150
      const startBlock = await client.getBlockNumber()
      const lastAllowedBlock = startBlock + maxBlockSpan
      const expectedMiners = new Set<Address>()
      const unexpectedMiners = new Set<Address>()
      const feeMiners = new Set<Address>()
      const feeBlocks = new Map<bigint, { miner: Address; totalFee: bigint; txs: number }>()
      let txsSent = 0
      let lastSampledBlock = startBlock

      const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

      const recordMiner = async (blockNumber: bigint) => {
        const block = await client.getBlock({ blockNumber })
        if (expectedRecipientSet.has(block.miner)) {
          expectedMiners.add(block.miner)
        } else {
          unexpectedMiners.add(block.miner)
        }
        return block.miner
      }

      const scanBlocksThrough = async (blockNumber: bigint) => {
        while (lastSampledBlock < blockNumber) {
          lastSampledBlock += 1n
          await recordMiner(lastSampledBlock)
        }
      }

      const submitPacedBatch = async () => {
        let nonce = await client.getTransactionCount({ address: sender.account.address, blockTag: 'pending' })
        const hashes: Hash[] = []
        for (let i = 0; i < txsPerBatch && lastSampledBlock < lastAllowedBlock; i++) {
          txsSent += 1
          const hash = await sender.sendTransaction({
            to: sender.account.address,
            value: 1n,
            maxFeePerGas: parseGwei('1000'),
            maxPriorityFeePerGas: parseGwei('10'),
            nonce,
          })
          nonce += 1
          hashes.push(hash)

          await sleep(txIntervalMs)
          const latestBlock = await client.getBlockNumber()
          await scanBlocksThrough(latestBlock > lastAllowedBlock ? lastAllowedBlock : latestBlock)
        }

        for (const hash of hashes) {
          const receipt = await client.waitForTransactionReceipt({ hash })
          await scanBlocksThrough(receipt.blockNumber)
          const miner = await recordMiner(receipt.blockNumber)
          if (expectedRecipientSet.has(miner)) {
            feeMiners.add(miner)
          }

          const feeBlock = feeBlocks.get(receipt.blockNumber) ?? { miner, totalFee: 0n, txs: 0 }
          expect(feeBlock.miner).to.equal(miner, `block ${receipt.blockNumber} miner changed while aggregating fees`)
          feeBlock.totalFee += receipt.gasUsed * receipt.effectiveGasPrice
          feeBlock.txs += 1
          feeBlocks.set(receipt.blockNumber, feeBlock)
        }
      }

      while (feeMiners.size < expectedValidatorCount && lastSampledBlock < lastAllowedBlock) {
        await submitPacedBatch()
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
      const feeMinerSummary = [...feeMiners].join(', ')
      const unexpectedMinerSummary = [...unexpectedMiners].join(', ')

      expect(unexpectedMiners.size).to.equal(
        0,
        [
          `Unexpected miners observed: ${unexpectedMinerSummary}.`,
          `Expected only LOCALDEV_FEE_RECIPIENTS: ${[...expectedRecipientSet].join(', ')}.`,
        ].join(' '),
      )

      for (const [blockNumber, feeBlock] of feeBlocks) {
        const [minerBefore, minerAfter] = await Promise.all([
          client.getBalance({ address: feeBlock.miner, blockNumber: blockNumber - 1n }),
          client.getBalance({ address: feeBlock.miner, blockNumber }),
        ])
        expect(
          minerAfter - minerBefore,
          `miner ${feeBlock.miner} should receive tx fees in block ${blockNumber} (${feeBlock.txs} txs)`,
        ).to.equal(feeBlock.totalFee)
      }

      expect(expectedMiners.size).to.equal(
        expectedValidatorCount,
        [
          `Expected all ${expectedValidatorCount} proposers to produce blocks`,
          `within ${maxBlockSpan} blocks;`,
          `observed ${expectedMiners.size} expected (${expectedMinerSummary}) after ${txsSent} txs over a ${observedBlockSpan}-block span.`,
        ].join(' '),
      )

      expect(feeMiners.size).to.equal(
        expectedValidatorCount,
        [
          `Expected all ${expectedValidatorCount} proposers to produce fee-bearing blocks`,
          `within ${maxBlockSpan} blocks;`,
          `observed ${feeMiners.size} expected (${feeMinerSummary}) after ${txsSent} txs over a ${observedBlockSpan}-block span.`,
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
