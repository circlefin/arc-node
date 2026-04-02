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
import { Hash, parseEther } from 'viem'
import { PublicClient, WalletClient } from '@nomicfoundation/hardhat-viem/types'
import { Denylist, getClients, ReceiptVerifier } from '../helpers'

describe('Denylist', () => {
  /**
   * Denylists `target`, verifies contract state for both `target` and `other`,
   * asserts the given `sendTx` is rejected at the txpool level, then cleans up.
   */
  const denylistAndExpectRejection = async (
    client: PublicClient,
    operator: WalletClient,
    denylistTarget: WalletClient,
    nonDenylistTarget: WalletClient,
    sendTx: () => Promise<Hash>,
  ) => {
    const denylistContract = Denylist.attach(operator)
    const denylistRead = Denylist.attach(client).read

    const denylistReceipt = await denylistContract.write
      .denylist([[denylistTarget.account.address]])
      .then((hash) => ReceiptVerifier.waitSuccess(hash))

    denylistReceipt.verifyEvents((ev) => {
      ev.expectDenylisted({ account: denylistTarget.account.address })
      ev.expectAllEventsMatched()
    })

    expect(await denylistRead.isDenylisted([denylistTarget.account.address])).to.be.true
    expect(await denylistRead.isDenylisted([nonDenylistTarget.account.address])).to.be.false

    await expect(sendTx()).to.be.rejectedWith(/is denylisted/)

    const unDenylistReceipt = await denylistContract.write
      .unDenylist([[denylistTarget.account.address]])
      .then((hash) => ReceiptVerifier.waitSuccess(hash))

    unDenylistReceipt.verifyEvents((ev) => {
      ev.expectUnDenylisted({ account: denylistTarget.account.address })
      ev.expectAllEventsMatched()
    })
  }

  it('txpool rejects transaction from denylisted sender', async () => {
    const { client, operator, createRandWallet } = await getClients()
    const sender = await createRandWallet(parseEther('0.1'))
    const receiver = await createRandWallet()

    await denylistAndExpectRejection(client, operator, sender, receiver, () =>
      sender.sendTransaction({ to: receiver.account.address, value: parseEther('0.001') }),
    )
  })

  it('txpool rejects transaction to denylisted recipient', async () => {
    const { client, operator, createRandWallet } = await getClients()
    const sender = await createRandWallet(parseEther('0.1'))
    const recipient = await createRandWallet()

    await denylistAndExpectRejection(client, operator, recipient, sender, () =>
      sender.sendTransaction({ to: recipient.account.address, value: parseEther('0.001') }),
    )
  })
})
