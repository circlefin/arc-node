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
import { createWalletClient, getChain } from '../../../scripts/hardhat/viem-helper'
import { LocalDevAccountCreator } from '../../../scripts/genesis/AccountCreator'
import { localdevFeeRecipient } from '../../../scripts/genesis'
import { Address } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { expect } from 'chai'

// Re-export the canonical fee recipient from scripts/genesis/addresses.ts.
// Used as genesis coinbase and cl_suggested_fee_recipient across all localdev validators.
export const LOCALDEV_FEE_RECIPIENT: Address = localdevFeeRecipient

// Per-validator fee recipients used by `crates/quake/scenarios/localdev.toml`.
// Index `i` corresponds to `validator${i + 1}` in the manifest.
export const LOCALDEV_FEE_RECIPIENTS: readonly Address[] = [
  '0x1111111111111111111111111111111111111111',
  '0x2222222222222222222222222222222222222222',
  '0x3333333333333333333333333333333333333333',
  '0x4444444444444444444444444444444444444444',
  '0x5555555555555555555555555555555555555555',
] as const

/**
 * Get the clients for the localdev network
 * @returns The clients for the localdev network
 */
export const getClients = async () => {
  const accountCreator = new LocalDevAccountCreator()
  const chain = getChain(hre)
  const client = await hre.viem.getPublicClient({
    chain,
    pollingInterval: 50,
    cacheTime: 0,
  })
  const accounts = await hre.viem.getWalletClients({ chain })
  const namedAccounts = accountCreator.namedAccounts(accounts)

  // the controllers in genesis, indexed by registrationID
  const getController = (registrationId: bigint, existing = true) =>
    createWalletClient(hre, accountCreator.getController(registrationId, existing))

  const createRandWallet = async (initAmount: bigint = 0n) => {
    const wallet = createWalletClient(hre, privateKeyToAccount(generatePrivateKey()))
    if (initAmount > 0n) {
      // Fund the random sender
      await namedAccounts.sender
        .sendTransaction({ to: wallet.account.address, value: initAmount })
        .then((hash) => client.waitForTransactionReceipt({ hash }))
        .then((receipt) => {
          expect(receipt.status).to.be.eq('success')
          return receipt
        })
    }
    return wallet
  }

  return { chain, client, getController, createRandWallet, ...namedAccounts }
}

/**
 * Get the validators for the localdev network
 * @returns The validators for the localdev network
 */
export const getValidators = async () => new LocalDevAccountCreator().validators()
