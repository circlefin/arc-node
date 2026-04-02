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

import { Account, Address, Chain, fromHex, parseAbi, PublicClient, RpcSchema, Transport } from 'viem'
import { nativeCoinAutorityAddress } from '../../scripts/genesis'

export class NativeCoinAuthority {
  static readonly address: Address = nativeCoinAutorityAddress

  static readonly abi = parseAbi([
    'function mint(address to, uint256 amount) external returns (bool)',
    'function burn(address from, uint256 amount) external returns (bool)',
    'function transfer(address from, address to, uint256 amount) external returns (bool)',
    'event NativeCoinMinted(address indexed recipient, uint256 amount)',
    'event NativeCoinBurned(address indexed from, uint256 amount)',
    'event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount)',
  ])

  static totalSupply = async <
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    R extends RpcSchema | undefined,
  >(
    client: PublicClient<T, C, A, R>,
  ) => {
    const value = await client.getStorageAt({
      address: NativeCoinAuthority.address,
      slot: '0x0000000000000000000000000000000000000000000000000000000000000002',
    })
    return fromHex(value ?? '0x0', 'bigint')
  }
}
