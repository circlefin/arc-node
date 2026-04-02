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
import { Account, Chain, Hash, PublicClient, RpcSchema, Transport } from 'viem'
import { AddressOrAccount, parseAddress } from './matchers'

type GetBalanceHook = () => Promise<bigint>

/**
 * Take a snapshot of the balances of the addresses
 * @param client The client to use
 * @param addresses The addresses to take the snapshot of
 * @returns The snapshot of the balances
 */
export const balancesSnapshot = async <
  M extends { [key: string]: AddressOrAccount | GetBalanceHook },
  T extends Transport,
  C extends Chain | undefined,
  A extends Account | undefined,
  R extends RpcSchema | undefined,
  O extends { [key in keyof M]: bigint },
>(
  client: PublicClient<T, C, A, R>,
  addresses: M,
) => {
  const takeSnapshot = async () => {
    const entries = Object.entries(addresses)
    const balances = await Promise.all(
      entries.map(([_, address]) =>
        typeof address === 'function' ? address() : client.getBalance({ address: parseAddress(address) }),
      ),
    )
    return Object.fromEntries(entries.map(([name, _], i) => [name, balances[i]])) as O
  }
  return new BalanceComparator(await takeSnapshot(), takeSnapshot)
}

/**
 * A class to compare the balances of the addresses
 */
export class BalanceComparator<S extends { [key: string]: bigint }> {
  private _decr?: Partial<S>
  private _incr?: Partial<S>
  constructor(
    private before: S,
    private readonly _takeSnapshot: () => Promise<S>,
  ) {}

  increase = (amounts: Partial<S>) => {
    this._incr = { ...this._incr, ...amounts }
    return this
  }

  decrease = (amounts: Partial<S>) => {
    this._decr = { ...this._decr, ...amounts }
    return this
  }

  state = () => {
    return { ...this.before }
  }

  update = async () => {
    this.before = await this._takeSnapshot()
    this._incr = {}
    this._decr = {}
  }

  verifyWithOverride = async (
    override: Partial<{
      [key in keyof S]: bigint | ((before: S, after: S) => bigint)
    }> = {},
    hash?: Hash,
  ) => {
    const _after = await this._takeSnapshot()
    for (const key of Object.keys(this.before)) {
      const before = this.before[key] ?? 0n
      const after = _after[key] ?? 0n
      const incr = this._incr?.[key] ?? 0n
      const decr = this._decr?.[key] ?? 0n
      if (key in override) {
        const v = override[key]
        const value: bigint | undefined = typeof v === 'function' ? v(this.before, _after) : v
        expect(value, `tx ${hash}, balance ${key}: (${before} + ${incr} - ${decr}) => ${value} != ${after}`).to.be.eq(
          after,
        )
        continue
      }
      expect(before + incr - decr, `tx ${hash}, balance ${key}: ${before} + ${incr} - ${decr} != ${after}`).to.be.eq(
        after,
      )
    }
    this.before = _after
    this._incr = {}
    this._decr = {}
  }

  verify = async (hash?: Hash) => this.verifyWithOverride({}, hash)
}
