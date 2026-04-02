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

import { AssertionError, expect } from 'chai'
import { skipCompare } from './skippable'
import { toJsonString } from './utils'
import * as ed from '@noble/ed25519'

describe('matchers', () => {
  describe('bigint', () => {
    it('should support bigint comparison', () => {
      expect(1n).to.eq(1n)
      expect(1n).to.lt(2n)
      expect(1n).to.gt(-3n)
      expect(1n).to.be.above(0n)
    })

    it('support deep eql', () => {
      expect({ a: 1n }).to.deep.eq({ a: 1n })
    })

    it('support hex to bigint, but not bigint to hex comparison', () => {
      expect(13213n).to.not.eq('0x339d')
      expect('0x3').to.eq(3n)
    })

    it('support bigint in the length chainable context', () => {
      expect([1, 2, 3]).to.be.length(3).but.not.eq(4n)
    })

    it('support within without type definition', () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any,@typescript-eslint/no-unsafe-argument
      expect(3n).to.within(1n as any, 5n as any)
    })

    it('not support closeTo and approximately', () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any,@typescript-eslint/no-unsafe-argument
      expect(() => expect(3n).to.closeTo(4n as any, 1n as any)).to.throw(AssertionError)
      // eslint-disable-next-line @typescript-eslint/no-explicit-any,@typescript-eslint/no-unsafe-argument
      expect(() => expect(3n).to.approximately(4n as any, 1n as any)).to.throw(AssertionError)
    })
  })

  describe('hexEqual', () => {
    it('should support hexEqual case insensitive', () => {
      expect('0x1234').to.be.hexEqual('0x1234')
      expect('0x1234').to.not.be.hexEqual('0x1235')
      expect('0x001234').to.not.be.hexEqual('0x1234')
      expect('0xAE').to.be.hexEqual('0xae')
    })

    it('always fail if the value is not a hex string', () => {
      expect(() => expect(1n).to.be.hexEqual('0x3')).to.throw(AssertionError, '"1" is not a valid hex string')
      expect(() => expect(1n).to.not.be.hexEqual('0x3')).to.throw(AssertionError, '"1" is not a valid hex string')
      expect(() => expect('ae47').to.be.hexEqual('0x3')).to.throw(AssertionError, '"ae47" is not a valid hex string')
      expect(() => expect('ae47').to.not.be.hexEqual('0x3')).to.throw(
        AssertionError,
        '"ae47" is not a valid hex string',
      )
    })
  })

  describe('addressEqual', () => {
    it('should support account and wallet client', () => {
      expect('0x4e59b44847b379578588920cA78FbF26c0B4956C').to.be.addressEqual(
        '0x4e59b44847b379578588920ca78fbf26c0b4956c',
      )
      expect('0x4e59b44847b379578588920cA78FbF26c0B4956C').to.be.addressEqual({
        address: '0x4e59b44847b379578588920ca78fbf26c0b4956c',
      })
      expect('0x4e59b44847b379578588920cA78FbF26c0B4956C').to.be.addressEqual({
        account: { address: '0x4e59b44847b379578588920ca78fbf26c0b4956c' },
      })
    })

    it('always fail if the value is not an addressable value', () => {
      expect(() => expect('0x3').to.be.addressEqual('0x3', 'test prefix')).to.throw(
        AssertionError,
        /test prefix.*"0x3" is not a valid address or account/,
      )
      expect(() => expect('0x3').to.not.be.addressEqual('0x3', 'test prefix')).to.throw(
        AssertionError,
        /test prefix.*"0x3" is not a valid address or account/,
      )
    })
  })

  describe('skippable ', () => {
    it('skip compare', () => {
      expect('0x3').to.eq(skipCompare)
      expect('0x4e59b44847b379578588920cA78FbF26c0B4956C').to.be.eq(skipCompare)
      expect({ a: '0x3', b: 4n }).to.be.deep.eq(skipCompare)
    })

    it('not skippable in deep compare', () => {
      expect({ a: '0x3', b: 4n }).to.be.not.deep.eq({ a: '0x3', b: skipCompare })
    })
  })
})

describe('toJsonString', () => {
  it('basic cases', () => {
    expect(toJsonString({ a: '0x1234' })).to.be.eq('{"a":"0x1234"}')
  })

  it('should support bigint', () => {
    expect(toJsonString({ a: 1n })).to.be.eq('{"a":"1"}')
  })

  it('should support bytes array', async () => {
    expect(toJsonString({ a: new Uint8Array([1, 2, 3]) })).to.be.eq('{"a":"0x010203"}')
    const key = Buffer.from('d18da290a2b45c0bcde575e5890f1cc23fd220041bead2a0e96acf1bf16509f6', 'hex')
    const pubkey = await ed.getPublicKeyAsync(Uint8Array.from(key))
    expect(toJsonString({ key })).to.be.eq(
      '{"key":{"type":"Buffer","data":[209,141,162,144,162,180,92,11,205,229,117,229,137,15,28,194,63,210,32,4,27,234,210,160,233,106,207,27,241,101,9,246]}}',
    )
    expect(toJsonString({ pubkey })).to.be.eq(
      '{"pubkey":"0xf939deac3bccfa639a712f5d3fe0691699ea52bb265c06ae3a1b56e2bc3bc9ef"}',
    )
  })
})
