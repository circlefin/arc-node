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
import hre from 'hardhat'
import { getChain } from '../../scripts/hardhat/viem-helper'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { encodeFunctionData, parseAbi, zeroAddress } from 'viem'
import { multicall3Address } from '../../scripts/genesis'
import { PermissionedValidatorManager, ValidatorRegistry } from '../helpers/ValidatorManager'

describe('ValidatorRegistry simulation', () => {
  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const validatorRegistry = ValidatorRegistry.attach(client)
    const randomWallet = privateKeyToAccount(generatePrivateKey())
    const extraAbi = parseAbi(['function upgradeTo(address newImplementation)'])

    return { client, randomWallet, validatorRegistry, extraAbi }
  }

  it('migrate contract', async () => {
    const { client, validatorRegistry, extraAbi } = await clients()
    const proxyAdmin = await validatorRegistry.read.admin()
    expect(proxyAdmin).to.not.eq(zeroAddress)
    const res = await client.simulateCalls({
      account: proxyAdmin,
      calls: [
        {
          to: ValidatorRegistry.address,
          data: encodeFunctionData({ abi: extraAbi, functionName: 'upgradeTo', args: [multicall3Address] }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
  })
})

describe('PermissionedValidatorManager simulation', () => {
  const clients = async () => {
    const client = await hre.viem.getPublicClient({
      chain: getChain(hre),
    })
    const poaManager = PermissionedValidatorManager.attach(client)
    const randomWallet = privateKeyToAccount(generatePrivateKey())
    const extraAbi = parseAbi(['function upgradeTo(address newImplementation)'])

    return { client, randomWallet, poaManager, extraAbi }
  }

  it('migrate contract', async () => {
    const { client, poaManager, extraAbi } = await clients()
    const proxyAdmin = await poaManager.read.admin()
    expect(proxyAdmin).to.not.eq(zeroAddress)
    const res = await client.simulateCalls({
      account: proxyAdmin,
      calls: [
        {
          to: PermissionedValidatorManager.address,
          data: encodeFunctionData({ abi: extraAbi, functionName: 'upgradeTo', args: [multicall3Address] }),
        },
      ],
    })
    expect(res.results[0].status).to.be.eq('success')
  })
})
