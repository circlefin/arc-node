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
const maxUint64 = 18446744073709551615n

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
}

describe('ProtocolConfig genesis schema', () => {
  it('accepts a positive block gas limit', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...protocolConfig,
        feeParams: {
          ...protocolConfig.feeParams,
          blockGasLimit: 1n,
        },
      }),
    ).to.not.throw()
  })

  it('rejects a zero block gas limit', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...protocolConfig,
        feeParams: {
          ...protocolConfig.feeParams,
          blockGasLimit: 0n,
        },
      }),
    ).to.throw()
  })

  it('rejects block gas limits above uint64', () => {
    expect(() =>
      schemaProtocolConfig.parse({
        ...protocolConfig,
        feeParams: {
          ...protocolConfig.feeParams,
          blockGasLimit: maxUint64 + 1n,
        },
      }),
    ).to.throw()
  })
})
