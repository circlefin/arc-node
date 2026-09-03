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

const protocolConfig = {
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
  it('accepts consensusParams at the uint16 upper bound', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...protocolConfig,
        consensusParams: {
          ...validConsensusParams,
          timeoutProposeMs: 65535n,
        },
      }),
    ).to.not.throw()
  })

  it('rejects consensusParams values above the uint16 upper bound', () => {
    for (const key of Object.keys(validConsensusParams) as Array<keyof typeof validConsensusParams>) {
      expect(
        () =>
          schemaProtocolConfig.parse({
            ...protocolConfig,
            consensusParams: {
              ...validConsensusParams,
              [key]: 65536n,
            },
          }),
        `${key} should reject 65536`,
      ).to.throw()
    }
  })
})
