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
import { Account, Address, getAddress, isAddress } from 'viem'
import { isSkipCompare, SkipCompare } from './skippable'

export type AddressOrAccount = Address | { account: Account } | { address: Address }

/**
 * Parse an address, account or wallet client to an address
 * @param addressOrAccount The address, account or wallet client to parse
 * @returns The address
 */
export function parseAddress(addressOrAccount: undefined): undefined
export function parseAddress(addressOrAccount: AddressOrAccount): Address
export function parseAddress(addressOrAccount?: AddressOrAccount): Address | undefined
export function parseAddress(addressOrAccount?: AddressOrAccount): Address | undefined {
  if (addressOrAccount == null) {
    return undefined
  }
  if (typeof addressOrAccount === 'string') {
    return getAddress(addressOrAccount)
  }
  if ('address' in addressOrAccount) {
    return getAddress(addressOrAccount.address)
  }
  if ('account' in addressOrAccount && 'address' in addressOrAccount.account) {
    return getAddress(addressOrAccount.account.address)
  }
  throw new Error('Invalid address or account')
}

export function isAddressable(addressOrAccount: unknown): addressOrAccount is AddressOrAccount {
  if (typeof addressOrAccount === 'string') {
    return isAddress(addressOrAccount)
  }
  if (typeof addressOrAccount === 'object' && addressOrAccount != null) {
    if ('address' in addressOrAccount && typeof addressOrAccount.address === 'string') {
      return isAddress(addressOrAccount.address)
    }
    if (
      'account' in addressOrAccount &&
      typeof addressOrAccount.account === 'object' &&
      addressOrAccount.account != null &&
      'address' in addressOrAccount.account &&
      typeof addressOrAccount.account.address === 'string'
    ) {
      return isAddress(addressOrAccount.account.address)
    }
  }
  return false
}

export const expectAddressEq = (
  target?: AddressOrAccount,
  expected?: AddressOrAccount | SkipCompare,
  desc?: string,
) => {
  if (isSkipCompare(expected)) {
    return
  }
  expect(parseAddress(target), desc).to.be.eq(parseAddress(expected))
}

export function supportAddressEqual(Assertion: Chai.AssertionStatic, utils: Chai.ChaiUtils) {
  function tryParseAddress(addressOrAccount: unknown): Address | undefined {
    try {
      return parseAddress(addressOrAccount as AddressOrAccount)
    } catch (_error) {
      return undefined
    }
  }

  function addressEqual(this: Chai.AssertionPrototype, other: unknown, message: string = '') {
    const subject = utils.flag(this, 'object') as unknown
    // eslint-disable-next-line @typescript-eslint/no-explicit-any, @typescript-eslint/no-unsafe-member-access
    const isNegated = (this as any).__flags.negate === true

    // check that both values are proper address strings
    for (const element of [subject, other]) {
      if (!isAddressable(element)) {
        this.assert(
          isNegated, // trick to make this assertion always fail
          // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
          `${message} Expected "${subject}" to be a hex string equal to "${other}", but "${element}" is not a valid address or account`,
          // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
          `${message} Expected "${subject}" not to be a hex string equal to "${other}", but "${element}" is not a valid address or account`,
          subject,
          other,
        )
      }
    }

    this.assert(
      tryParseAddress(subject) === tryParseAddress(other),
      // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
      `${message} Expected "${subject}" to be equal to "${other}"`,
      // eslint-disable-next-line @typescript-eslint/restrict-template-expressions
      `${message} Expected "${subject}" not to be equal to "${other}"`,
      subject,
      other,
    )
  }

  Assertion.addMethod('addressEqual', addressEqual)
}
