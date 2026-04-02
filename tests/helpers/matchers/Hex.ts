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

import { isHex } from 'viem'

/**
 * Reference the implementation of hexEqual in hardhat-chai-matchers.
 * - Use isHex from viem instead
 * - Only allow case insensitive comparison, do not skip leading zeros
 *
 * https://github.com/NomicFoundation/hardhat/blob/hardhat%402.26.2/packages/hardhat-chai-matchers/src/internal/hexEqual.ts
 */
export function supportHexEqual(Assertion: Chai.AssertionStatic, utils: Chai.ChaiUtils) {
  Assertion.addMethod('hexEqual', function (this: Chai.AssertionPrototype, other: unknown, message: string = '') {
    const subject = utils.flag(this, 'object') as unknown
    // eslint-disable-next-line @typescript-eslint/no-explicit-any, @typescript-eslint/no-unsafe-member-access
    const isNegated = (this as any).__flags.negate === true

    // check that both values are proper hex strings
    for (const element of [subject, other]) {
      if (!isHex(element)) {
        this.assert(
          isNegated, // trick to make this assertion always fail
          // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
          `${message} Expected "${subject}" to be a hex string equal to "${other}", but "${element}" is not a valid hex string`,
          // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
          `${message} Expected "${subject}" not to be a hex string equal to "${other}", but "${element}" is not a valid hex string`,
          subject,
          other,
        )
      }
    }

    // compare values
    this.assert(
      (subject as string).toLowerCase() === (other as string).toLowerCase(),
      // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
      `${message} Expected "${subject}" to be a hex string equal to "${other}"`,
      // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
      `${message} Expected "${subject}" NOT to be a hex string equal to "${other}", but it was`,
      subject,
      other,
    )
  })
}
