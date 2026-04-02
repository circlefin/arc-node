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
 * reference https://github.com/NomicFoundation/hardhat/blob/hardhat%402.26.2/packages/hardhat-chai-matchers/src/internal/bigNumber.ts
 * to add the bigint comparison support to chai
 */
export function supportBigNumber(Assertion: Chai.AssertionStatic, chaiUtils: Chai.ChaiUtils) {
  const equalsFunction = override('eq', 'equal', 'not equal', chaiUtils)
  Assertion.overwriteMethod('equals', equalsFunction)
  Assertion.overwriteMethod('equal', equalsFunction)
  Assertion.overwriteMethod('eq', equalsFunction)

  const gtFunction = override('gt', 'be above', 'be at most', chaiUtils)
  Assertion.overwriteMethod('above', gtFunction)
  Assertion.overwriteMethod('gt', gtFunction)
  Assertion.overwriteMethod('greaterThan', gtFunction)

  const ltFunction = override('lt', 'be below', 'be at least', chaiUtils)
  Assertion.overwriteMethod('below', ltFunction)
  Assertion.overwriteMethod('lt', ltFunction)
  Assertion.overwriteMethod('lessThan', ltFunction)

  const gteFunction = override('gte', 'be at least', 'be below', chaiUtils)
  Assertion.overwriteMethod('least', gteFunction)
  Assertion.overwriteMethod('gte', gteFunction)
  Assertion.overwriteMethod('greaterThanOrEqual', gteFunction)

  const lteFunction = override('lte', 'be at most', 'be above', chaiUtils)
  Assertion.overwriteMethod('most', lteFunction)
  Assertion.overwriteMethod('lte', lteFunction)
  Assertion.overwriteMethod('lessThanOrEqual', lteFunction)

  Assertion.overwriteMethod('within', overrideWithin(chaiUtils))
}

function isBigNumber(value: unknown): value is bigint {
  return typeof value === 'bigint'
}

function isBigNumberComparable(value: unknown): value is bigint | number {
  return typeof value === 'bigint' || typeof value === 'number'
}

function getObjectLength(value: unknown): [number | undefined, 'size' | 'length'] {
  if (value instanceof Map || value instanceof Set) {
    return [value.size, 'size']
  }
  if (Array.isArray(value)) {
    return [value.length, 'length']
  }
  if (typeof value === 'string') {
    return [value.length, 'length']
  }
  if (value != null && typeof value === 'object' && 'length' in value && typeof value.length === 'number') {
    return [value.length, 'length']
  }
  return [undefined, 'length']
}

function normalizeToBigInt(source: bigint | number | string): bigint {
  if (typeof source === 'bigint') {
    return source
  }
  return BigInt(source)
}

type Methods = 'eq' | 'gt' | 'lt' | 'gte' | 'lte'

function override(method: Methods, name: string, negativeName: string, chaiUtils: Chai.ChaiUtils) {
  return (_super: (...args: unknown[]) => unknown) =>
    overwriteBigNumberFunction(method, name, negativeName, _super, chaiUtils)
}

function overwriteBigNumberFunction(
  functionName: Methods,
  readableName: string,
  readableNegativeName: string,
  _super: (...args: unknown[]) => unknown,
  chaiUtils: Chai.ChaiUtils,
) {
  return function (this: Chai.AssertionStatic, ...args: unknown[]) {
    const [actualArg, message] = args
    const expectedFlag = chaiUtils.flag(this, 'object') as unknown

    if (message !== undefined) {
      chaiUtils.flag(this, 'message', message)
    }

    function compare(method: Methods, lhs: bigint, rhs: bigint): boolean {
      if (method === 'eq') {
        return lhs === rhs
      } else if (method === 'gt') {
        return lhs > rhs
      } else if (method === 'lt') {
        return lhs < rhs
      } else if (method === 'gte') {
        return lhs >= rhs
      } else if (method === 'lte') {
        return lhs <= rhs
      } else {
        // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
        throw new Error(`Unknown comparison operation ${method}`)
      }
    }
    if (Boolean(chaiUtils.flag(this, 'doLength')) && isBigNumber(actualArg)) {
      const [length, sizeOrLength] = getObjectLength(expectedFlag)
      if (length === undefined) {
        _super.apply(this, args)
        return
      }
      const expected = normalizeToBigInt(length)
      const actual = normalizeToBigInt(actualArg)
      this.assert(
        compare(functionName, expected, actual),
        `expected #{this} to have a ${sizeOrLength} ${readableName.replace(
          'be ',
          '',
        )} ${actual.toString()} but got ${expected}`,
        `expected #{this} to have a ${sizeOrLength} ${readableNegativeName} ${actual.toString()}`,
        expected,
        actual,
      )
    } else if (isBigNumber(expectedFlag) || isBigNumber(actualArg)) {
      if ((isHex(expectedFlag) || isBigNumberComparable(expectedFlag)) && isBigNumberComparable(actualArg)) {
        const expected = normalizeToBigInt(expectedFlag)
        const actual = normalizeToBigInt(actualArg)
        this.assert(
          compare(functionName, expected, actual),
          `expected ${expected} to ${readableName} ${actual}.`,
          `expected ${expected} to ${readableNegativeName} ${actual}.`,
          actual.toString(),
          expected.toString(),
        )
      } else {
        _super.apply(this, args)
      }
    } else {
      _super.apply(this, args)
    }
  }
}

function overrideWithin(chaiUtils: Chai.ChaiUtils) {
  return (_super: (...args: unknown[]) => unknown) => overwriteBigNumberWithin(_super, chaiUtils)
}

function overwriteBigNumberWithin(_super: (...args: unknown[]) => unknown, chaiUtils: Chai.ChaiUtils) {
  return function (this: Chai.AssertionStatic, ...args: unknown[]) {
    const [startArg, finishArg] = args
    const expectedFlag = chaiUtils.flag(this, 'object') as unknown
    if (isBigNumber(expectedFlag) || isBigNumber(startArg) || isBigNumber(finishArg)) {
      if (isBigNumberComparable(expectedFlag) && isBigNumberComparable(startArg) && isBigNumberComparable(finishArg)) {
        const expected = normalizeToBigInt(expectedFlag)
        const start = normalizeToBigInt(startArg)
        const finish = normalizeToBigInt(finishArg)
        this.assert(
          start <= expected && expected <= finish,
          `expected ${expected} to be within ${start}..${finish}`,
          `expected ${expected} to not be within ${start}..${finish}`,
          expected,
          [start, finish],
        )
        return
      }
    }
    _super.apply(this, args)
  }
}
