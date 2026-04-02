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

import { HardhatRuntimeEnvironment } from 'hardhat/types'
import { Account, defineChain, createWalletClient as viemCreateWalletClient, http } from 'viem'
import { arcLocaldev } from './chains/localdev'
import { arcTestnet } from './chains/testnet'
import { arcDevnet } from './chains/devnet'
import { anvil, hardhat, localhost } from 'viem/chains'

const chainDefinitions = {
  // default networks for hardhat
  hardhat: hardhat,
  anvil: anvil,
  localhost: localhost,

  // customized networks
  localdev: arcLocaldev,
  testnet: arcTestnet,
  devnet: arcDevnet,
}

/**
 * hardhat-viem use the viem default networks, before we define arc to
 * https://github.com/wevm/viem/tree/main/src/chains/definitions
 * We need to load the chain manually.
 */
export const getChain = (hre: HardhatRuntimeEnvironment) => {
  const name = hre.network.name
  const id = hre.network.config.chainId
  const url = (hre.network.config as { url?: string }).url

  if (id == null) {
    throw new Error(`chain ID for network ${name} is required`)
  }
  if (url == null || url === '') {
    throw new Error(`url for network ${name} is required`)
  }

  return defineChain({
    ...(chainDefinitions[name as keyof typeof chainDefinitions] ?? localhost),
    // patch hardhat configurable fields
    ...(id != null ? { id } : undefined),
    ...(url != null ? { rpcUrls: { default: { http: [url] } } } : undefined),
  })
}

export const createWalletClient = <T extends Account>(hre: HardhatRuntimeEnvironment, account: T) =>
  viemCreateWalletClient({ account, chain: getChain(hre), transport: http() })
