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

export const skipCompare = { __skippable$symbol: Symbol('skip') }
export type SkipCompare = typeof skipCompare

export function supportSkippable(Assertion: Chai.AssertionStatic, utils: Chai.ChaiUtils) {
  Assertion.overwriteMethod('equals', override('eq', 'equal', utils))
  Assertion.overwriteMethod('equal', override('eq', 'equal', utils))
  Assertion.overwriteMethod('eq', override('eq', 'equal', utils))

  Assertion.overwriteMethod('above', override('gt', 'above', utils))
  Assertion.overwriteMethod('gt', override('gt', 'greater than', utils))

  Assertion.overwriteMethod('below', override('lt', 'below', utils))
  Assertion.overwriteMethod('lt', override('lt', 'less than', utils))

  Assertion.overwriteMethod('least', override('gte', 'at least', utils))
  Assertion.overwriteMethod('gte', override('gte', 'greater than or equal', utils))

  Assertion.overwriteMethod('most', override('lte', 'at most', utils))
  Assertion.overwriteMethod('lte', override('lte', 'less than or equal', utils))
}

type Methods = 'eq' | 'gt' | 'lt' | 'gte' | 'lte'

function override(method: Methods, name: string, utils: Chai.ChaiUtils) {
  return (_super: (...args: unknown[]) => unknown) => overwriteSkippableFunction(method, name, _super, utils)
}

export function isSkipCompare(value: unknown): value is SkipCompare {
  if (typeof value === 'object' && value != null && '__skippable$symbol' in value) {
    return value.__skippable$symbol === skipCompare.__skippable$symbol
  }
  return false
}

function overwriteSkippableFunction(
  _functionName: Methods,
  _readableName: string,
  _super: (...args: unknown[]) => unknown,
  chaiUtils: Chai.ChaiUtils,
) {
  return function (this: Chai.AssertionStatic, ...args: unknown[]) {
    const [actual] = args
    const expected = chaiUtils.flag(this, 'object') as unknown
    if (isSkipCompare(expected) || isSkipCompare(actual)) {
      return
    }
    _super.apply(this, args)
  }
}
