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
import { schemaProtocolConfig } from '../../scripts/genesis/ProtocolConfig'

const proxyAdmin = '0x0000000000000000000000000000000000000001'
const owner = '0x0000000000000000000000000000000000000002'
const controller = '0x0000000000000000000000000000000000000003'
const pauser = '0x0000000000000000000000000000000000000004'

const validConsensusParams = {
  timeoutProposeMs: 3000n,
  timeoutProposeDeltaMs: 500n,
  timeoutPrevoteMs: 1000n,
  timeoutPrevoteDeltaMs: 500n,
  timeoutPrecommitMs: 1000n,
  timeoutPrecommitDeltaMs: 500n,
  timeoutRebroadcastMs: 2000n,
  targetBlockTimeMs: 1000n,
}

const baseConfig = {
  proxy: {
    admin: proxyAdmin,
  },
  owner,
  controller,
  pauser,
  feeParams: {
    alpha: 1n,
    kRate: 1n,
    inverseElasticityMultiplier: 1n,
    minBaseFee: 0n,
    maxBaseFee: 1_000_000n,
    blockGasLimit: 30_000_000n,
  },
  consensusParams: validConsensusParams,
}

describe('ProtocolConfig genesis schema', () => {
  it('accepts a valid config', () => {
    expect(() => schemaProtocolConfig.parse(baseConfig)).to.not.throw()
  })

  it('rejects zero consensus timeouts that updateConsensusParams also rejects', () => {
    const timeoutKeys = [
      'timeoutProposeMs',
      'timeoutProposeDeltaMs',
      'timeoutPrevoteMs',
      'timeoutPrevoteDeltaMs',
      'timeoutPrecommitMs',
      'timeoutPrecommitDeltaMs',
      'timeoutRebroadcastMs',
    ] as const

    for (const key of timeoutKeys) {
      expect(
        () =>
          schemaProtocolConfig.parse({
            ...baseConfig,
            consensusParams: {
              ...validConsensusParams,
              [key]: 0n,
            },
          }),
        `${key} should reject 0`,
      ).to.throw()
    }
  })

  it('rejects consensus params above uint16 max to prevent packed-slot overflow', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...baseConfig,
        consensusParams: {
          ...validConsensusParams,
          timeoutProposeMs: 65536n,
        },
      }),
    ).to.throw()
  })

  it('rejects minBaseFee greater than maxBaseFee', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...baseConfig,
        feeParams: {
          ...baseConfig.feeParams,
          minBaseFee: 1000n,
          maxBaseFee: 1n,
        },
      }),
    ).to.throw()
  })
})
