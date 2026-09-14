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
import { zeroAddress } from 'viem'
import { schemaDenylist } from '../../scripts/genesis/Denylist'

const proxyAdmin = '0x1111111111111111111111111111111111111111'
const owner = '0x2222222222222222222222222222222222222222'
const denylisterA = '0x3333333333333333333333333333333333333333'
const denylisterB = '0x4444444444444444444444444444444444444444'

const baseConfig = {
  proxy: {
    admin: proxyAdmin,
  },
  owner,
  denylisters: [denylisterA, denylisterB],
}

describe('Denylist genesis schema', () => {
  it('accepts a valid config with denylisters', () => {
    expect(() => schemaDenylist.parse(baseConfig)).to.not.throw()
  })

  it('accepts a valid config with omitted denylisters', () => {
    const { denylisters: _, ...configWithoutDenylisters } = baseConfig
    expect(() => schemaDenylist.parse(configWithoutDenylisters)).to.not.throw()
  })

  it('accepts a valid config with empty denylisters array', () => {
    expect(() => schemaDenylist.parse({ ...baseConfig, denylisters: [] })).to.not.throw()
  })

  it('rejects zero address as owner', () => {
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      owner: zeroAddress,
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('Owner cannot be the zero address'))).to.be.true
    }
  })

  it('rejects zero address in denylisters', () => {
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      denylisters: [denylisterA, zeroAddress],
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('cannot be the zero address'))).to.be.true
    }
  })

  it('rejects duplicate denylisters', () => {
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      denylisters: [denylisterA, denylisterB, denylisterA],
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('must be unique'))).to.be.true
    }
  })

  it('rejects duplicate denylisters with mixed casing', () => {
    const denylisterAUpper = `0x${denylisterA.slice(2).toUpperCase()}` as const
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      denylisters: [denylisterA, denylisterAUpper],
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('must be unique'))).to.be.true
    }
  })

  it('rejects when owner is the same as proxy admin', () => {
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      owner: proxyAdmin,
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('cannot be the same as the proxy admin'))).to.be
        .true
    }
  })

  it('rejects when a denylister is the same as proxy admin', () => {
    const result = schemaDenylist.safeParse({
      ...baseConfig,
      denylisters: [denylisterA, proxyAdmin],
    })

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.message.includes('cannot be the same as the proxy admin'))).to.be
        .true
    }
  })
})
