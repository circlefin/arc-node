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
import { encodeDeployData, getContract, parseEther, zeroAddress } from 'viem'
import { balancesSnapshot, DeterministicDeployerProxy, getClients, ReceiptVerifier } from '../helpers'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { WalletClient } from '@nomicfoundation/hardhat-viem/types'
import { CallHelper } from '../helpers/CallHelper'
import { expect } from 'chai'
import { recoverAuthorizationAddress, verifyAuthorization } from 'viem/utils'
import { encodeFunctionData } from 'viem'
import { createWalletClient } from '../../scripts/hardhat/viem-helper'
import { LocalDevAccountCreator } from '../../scripts/genesis/AccountCreator'

describe('eip-7702', () => {
  const initClients = async () => {
    const accountCreator = new LocalDevAccountCreator()
    const { sender: senderAccount } = accountCreator.namedAccounts(accountCreator.defaultAccounts())
    const { client, receiver, admin } = await getClients()

    // We need a pure viem wallet client here. Hardhat wallet will use the type 'json-rpc'.
    // It may not support the eip-7702 authorization list.
    const sender = createWalletClient(hre, senderAccount)

    const randSender = async (faucet: WalletClient, initAmount: bigint = parseEther('0.1')) => {
      const sender = createWalletClient(hre, privateKeyToAccount(generatePrivateKey()))

      // Fund the random sender
      if (initAmount > 0n) {
        await faucet
          .sendTransaction({ to: sender.account.address, value: initAmount })
          .then(ReceiptVerifier.waitSuccess)
      }
      return sender
    }
    const callHelper = await CallHelper.deterministicDeploy(sender, client)
    const nativeTransferHelperArtifact = await hre.artifacts.readArtifact('NativeTransferHelper')
    const addr = await DeterministicDeployerProxy.deployCode(
      sender,
      client,
      encodeDeployData({
        abi: nativeTransferHelperArtifact.abi,
        bytecode: nativeTransferHelperArtifact.bytecode,
        args: [zeroAddress, false],
      }),
    )
    const nativeTransferHelper = getContract({ abi: nativeTransferHelperArtifact.abi, address: addr, client })

    return { client, sender, receiver, admin, callHelper, nativeTransferHelper, randSender }
  }
  let clients: Awaited<ReturnType<typeof initClients>>
  before(async () => {
    clients = await initClients()
  })

  it('delegate happy path', async () => {
    const { client, sender, receiver, randSender, callHelper } = clients
    const wallet = await randSender(sender, 0n)

    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      wallet,
      senderNonce: () => client.getTransactionCount({ address: sender.account.address }).then(BigInt),
      walletNonce: () => client.getTransactionCount({ address: wallet.account.address }).then(BigInt),
    })

    const authorization = await wallet.signAuthorization({
      account: wallet.account,
      // delegate to callHelper is only for testing in local, it may lose all assets.
      contractAddress: callHelper.address,
    })

    expect(await verifyAuthorization({ address: wallet.account.address, authorization })).to.be.true
    expect(await recoverAuthorizationAddress({ authorization })).to.be.addressEqual(wallet.account.address)

    const code = await client.getCode({ address: wallet.account.address })
    expect(code).to.be.undefined

    const transferAmount = parseEther('0.1')
    const delegateReceipt = await sender
      .sendTransaction({
        to: wallet.account.address,
        value: transferAmount,
        authorizationList: [authorization],
      })
      .then(ReceiptVerifier.waitSuccess)
    delegateReceipt.verifyGasUsedApproximately(50255n).verifyEvents((ev) => {
      ev.expectCount(1).expectNativeTransfer({
        from: sender.account.address,
        to: wallet.account.address,
        amount: transferAmount,
      })
    })

    await balances
      .increase({ senderNonce: 1n, wallet: transferAmount, walletNonce: 1n })
      .decrease({ sender: transferAmount + delegateReceipt.totalFee() })
      .verify(delegateReceipt.transactionHash)

    const delegateCode = await client.getCode({ address: wallet.account.address })
    expect(delegateCode).to.be.not.undefined

    const batchReceipt = await sender
      .sendTransaction({
        to: wallet.account.address,
        data: CallHelper.encodeNested({
          fn: 'executeBatch',
          calls: [
            {
              target: wallet.account.address,
              data: { fn: 'transfer', to: receiver.account.address, value: 100n },
            },
            {
              target: wallet.account.address,
              data: { fn: 'transfer', to: sender.account.address, value: transferAmount - 100n },
            },
          ],
        }),
      })
      .then(ReceiptVerifier.waitSuccess)

    await balances
      .increase({ senderNonce: 1n, sender: transferAmount - 100n, receiver: 100n })
      .decrease({ sender: batchReceipt.totalFee(), wallet: transferAmount })
      .verify(batchReceipt.transactionHash)
  })

  it('delegate then destroy', async () => {
    const { client, sender, receiver, randSender, nativeTransferHelper } = clients
    const wallet = await randSender(sender, 0n)

    const transferAmount = parseEther('0.03')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      wallet,
      senderNonce: () => client.getTransactionCount({ address: sender.account.address }).then(BigInt),
      walletNonce: () => client.getTransactionCount({ address: wallet.account.address }).then(BigInt),
    })

    const authorization = await wallet.signAuthorization({
      account: wallet.account,
      // delegate to callHelper is only for testing in local, it may lose all assets.
      contractAddress: nativeTransferHelper.address,
    })

    const receipt = await sender
      .sendTransaction({
        account: sender.account,
        to: wallet.account.address,
        value: transferAmount,
        data: encodeFunctionData({
          abi: nativeTransferHelper.abi,
          functionName: 'triggerSelfDestruct',
          args: [receiver.account.address],
        }),
        authorizationList: [authorization],
      })
      .then(ReceiptVerifier.waitSuccess)

    await balances
      .decrease({ sender: transferAmount + receipt.totalFee() })
      .increase({ receiver: transferAmount, senderNonce: 1n, walletNonce: 1n })
      .verify(receipt.transactionHash)

    const code = await client.getCode({ address: wallet.account.address })
    expect(code).to.be.not.undefined
  })

  it('delegate then destruct and transfer', async () => {
    const { client, sender, receiver, randSender, callHelper } = clients
    const wallet = await randSender(sender, 0n)

    const transferAmount = parseEther('0.03')
    const amount1 = parseEther('0.01')
    const amount2 = parseEther('0.02')
    const balances = await balancesSnapshot(client, {
      sender,
      receiver,
      wallet,
      senderNonce: () => client.getTransactionCount({ address: sender.account.address }).then(BigInt),
      walletNonce: () => client.getTransactionCount({ address: wallet.account.address }).then(BigInt),
    })

    const authorization = await wallet.signAuthorization({
      account: wallet.account,
      // delegate to callHelper is only for testing in local, it may lose all assets.
      contractAddress: callHelper.address,
    })

    const receipt = await sender
      .sendTransaction({
        account: sender.account,
        to: callHelper.address,
        value: transferAmount,
        data: CallHelper.encodeNested({
          fn: 'executeBatch',
          calls: [
            {
              target: wallet.account.address,
              value: amount1,
              data: encodeFunctionData({
                abi: CallHelper.abi,
                functionName: 'triggerSelfDestruct',
                args: [receiver.account.address],
              }),
            },
            {
              // Transfer to wallet after destroy, the balance burned.
              target: wallet.account.address,
              value: amount2,
            },
          ],
        }),
        authorizationList: [authorization],
      })
      .then(ReceiptVerifier.waitSuccess)

    await balances
      .decrease({ sender: transferAmount + receipt.totalFee() })
      .increase({ receiver: amount1, wallet: amount2, senderNonce: 1n, walletNonce: 1n })
      .verify(receipt.transactionHash)

    const code = await client.getCode({ address: wallet.account.address })
    expect(code).to.be.not.undefined
  })
})
