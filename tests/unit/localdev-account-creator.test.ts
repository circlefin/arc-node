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
import { LocalDevAccountCreator } from '../../scripts/genesis/AccountCreator'

const publicKey = `0x${'11'.repeat(32)}`

describe('LocalDevAccountCreator', () => {
  describe('overridePublicKeys', () => {
    it('accepts canonical decimal validator IDs', () => {
      const creator = new LocalDevAccountCreator({ overridePublicKeys: `1:${publicKey},42:${publicKey}` })

      expect(creator.overridePublicKeys.get(1)).to.equal(publicKey)
      expect(creator.overridePublicKeys.get(42)).to.equal(publicKey)
    })

    it('rejects non-canonical validator IDs', () => {
      for (const id of ['1.5', '1e0', '01', '-1', 'NaN', 'Infinity', '']) {
        expect(
          () => new LocalDevAccountCreator({ overridePublicKeys: `${id}:${publicKey}` }),
          `${id} should be rejected`,
        ).to.throw(/Invalid validator ID|missing ID/)
      }
    })
  })
})
