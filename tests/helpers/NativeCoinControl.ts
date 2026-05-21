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

import { Address, Hex, parseAbi, PublicClient, RpcSchema, Transport, Chain, Account, toHex } from 'viem'
import { nativeCoinControlAddress } from '../../scripts/genesis'
import { slotForAddressMap } from '../../scripts/genesis/types'

const BLOCKLIST_MAPPING_SLOT = 2n
const UNBLOCKLISTED_STATUS: Hex = toHex(0n, { size: 32 })

export const ERR_BLOCKED_ADDRESS = /Blocked address/

export class NativeCoinControl {
  static readonly address: Address = nativeCoinControlAddress

  static readonly abi = parseAbi([
    'function blocklist(address account) external returns (bool success)',
    'function isBlocklisted(address account) external view returns (bool _isBlocklisted)',
    'function unBlocklist(address account) external returns (bool success)',
    'event Blocklisted(address indexed account)',
    'event UnBlocklisted(address indexed account)',
  ])

  /**
   * State override that clears the blocklist entry for `address` in
   * NativeCoinControl's `isBlocklisted` mapping. Needed for simulations where
   * a blocklisted address (e.g. the NativeFiatToken contract, per genesis)
   * appears as the tx caller; the EVM handler's pre-execution sender check
   * would otherwise reject the tx.
   */
  static unblockStateOverride = (address: Address) => ({
    address: NativeCoinControl.address,
    stateDiff: [
      {
        slot: slotForAddressMap(BLOCKLIST_MAPPING_SLOT, address),
        value: UNBLOCKLISTED_STATUS,
      },
    ],
  })

  static isBlocklisted = async <
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    R extends RpcSchema | undefined,
  >(
    client: PublicClient<T, C, A, R>,
    account: Address,
  ): Promise<boolean> => {
    return await client.readContract({
      address: NativeCoinControl.address,
      abi: NativeCoinControl.abi,
      functionName: 'isBlocklisted',
      args: [account],
    })
  }
}
