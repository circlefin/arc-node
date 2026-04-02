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

import { WalletClient, PublicClient } from '@nomicfoundation/hardhat-viem/types'
import { Address, concat, fromHex, Hex, keccak256, toHex } from 'viem'
import { generatePrivateKey } from 'viem/accounts'
import { deterministicDeployerProxyAddress } from '../../scripts/genesis'

export class DeterministicDeployerProxy {
  static address: Address = deterministicDeployerProxyAddress

  static getDeployAddress(callData: Hex, salt: bigint = 0n): Address {
    return ('0x' +
      keccak256(
        concat(['0xff', DeterministicDeployerProxy.address, toHex(salt, { size: 32 }), keccak256(callData)]),
      ).slice(-40)) as Address
  }

  static getDeployData(callData: Hex, salt: bigint = 0n): Hex {
    return concat([toHex(salt, { size: 32 }), callData])
  }

  static findSalt(prefix: string, callData: Hex, iterations: number = 1000000) {
    let count = 0
    let salt = fromHex(generatePrivateKey(), 'bigint') - BigInt(iterations) / 2n
    if (salt < 0) {
      salt = 0n
    }
    while (prefix != null && count < iterations) {
      const address = DeterministicDeployerProxy.getDeployAddress(callData, salt)
      if (address.startsWith(prefix)) {
        return { address, salt }
      }
      salt++
      count++
    }
    throw new Error('Failed to find salt')
  }

  static async deployCode(
    wallet: WalletClient,
    client: PublicClient,
    callData: Hex,
    salt: bigint = 0n,
  ): Promise<Address> {
    const address = DeterministicDeployerProxy.getDeployAddress(callData, salt ?? 0n)
    const code2 = await client.getCode({ address })
    if (code2 != null) {
      return address
    }

    const receipt = await wallet
      .sendTransaction({
        to: DeterministicDeployerProxy.address,
        data: DeterministicDeployerProxy.getDeployData(callData, salt),
      })
      .then((hash) => client.waitForTransactionReceipt({ hash }))
    if (receipt.status !== 'success') {
      throw new Error('Deploy failed: transaction reverted')
    }

    const code = await client.getCode({ address })
    if (code === '0x') {
      throw new Error('Deploy failed: no code at the address')
    }
    return address
  }
}
