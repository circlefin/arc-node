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

import hre from 'hardhat'
import { getClients } from '../helpers'
import { parseGwei, toBlobs, stringToHex, TransactionExecutionError } from 'viem'
import { kzg } from '../helpers/kzg'
import { expect } from 'chai'
import { LocalDevAccountCreator } from '../../scripts/genesis/AccountCreator'
import { createWalletClient } from '../../scripts/hardhat/viem-helper'

describe('EIP-4844 blob tests', () => {
  const blobs = toBlobs({ data: stringToHex('this is a blob') })

  it('submit blob should be rejected from the mempool', async () => {
    const { client } = await getClients()
    const accountCreator = new LocalDevAccountCreator()
    // The hardhat wallet will use the type 'json-rpc'. We need a pure viem wallet client here.
    const walletOne = createWalletClient(hre, accountCreator.defaultAccounts()[0])

    await expect(
      walletOne.sendTransaction({
        blobs,
        kzg,
        maxFeePerBlobGas: parseGwei('30'),
        to: '0x0000000000000000000000000000000000000000',
        chain: client.chain,
      }),
    ).to.be.rejectedWith(TransactionExecutionError, /transaction type not supported/i)
  })

  it('blob base fee should be at the floor', async () => {
    const { client } = await getClients()

    const baseFee = await client.getBlobBaseFee()
    expect(baseFee).to.equal(1n)
  })
})
