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

import fs from 'fs'
import path from 'path'
import { Address, Hex, parseEther, parseGwei, zeroAddress } from 'viem'
import { createBuilderContext, buildGenesis, GenesisConfig } from '../../scripts/genesis'
import { bigintReplacer } from '../../scripts/genesis/types'

type Validator = {
  publicKey: Hex
  activateController: Address
  removeController: Address
}

const initialVotingPower = 2000n

const build = async () => {
  const ctx = await createBuilderContext({ network: 'mainnet', chainId: 5042 })

  // The sequences of validator registration must follow the order of the following array due to registrationId assignment.
  const validators: Validator[] = [
    {
      publicKey: '0x12a68aa84643efd6fb79b4097ef9b5ae1bef849c9c75e8eba2b977e09d227c35',
      activateController: '0x5f86a4dCBD49c86Becc4FB89680f66FeC8eF358a',
      removeController: '0x6F80738F31019B70B616Ac9Fac1C853dFC35Cc37',
    },
    {
      publicKey: '0xae89fa207bb808e8a34e05b5892a16625e8b66b92e597f8866aa586b487dfbfe',
      activateController: '0xb0100454798fD2a11d7e7Bde0804a29171d2F492',
      removeController: '0xA53dd220650550BC68dFC63faAF954b85eAfED7c',
    },
    {
      publicKey: '0xd5c96c5d0daf70f25fb5bfc20c0747b2ce0408cc6e54b8494f3a62b61ffca7cd',
      activateController: '0x64637C1f9899684722da4657d94396ec4Fd85E09',
      removeController: '0x882b0d0E0952170a7d3D62946327fF3F6d7AD329',
    },
    {
      publicKey: '0xed259c1abb397823afb31cb36089547367221242de3057df803a0b091e9a1c8d',
      activateController: '0x5c2220b253c4E02Eec51A3959b9f67E599E8ECF9',
      removeController: '0x386976F806739bDA4126a18eEF4157F92Fb53Cce',
    },
    {
      publicKey: '0xf43562191fe65ba250ac32d4602efb664586bf4e8150e4c80cf2dbf64650d2a9',
      activateController: '0xB0556E9AE15cA9e02fFD66C1651134bf343ad2B2',
      removeController: '0x5e887aFa6E659995A546e72B9959594D8DAEDD71',
    },
    {
      publicKey: '0xf927d6659307a219c3a59b51a31c29412b6d2b1c1717697a0ae0d5c9999ba740',
      activateController: '0x5802620312d16b94E70147AAf730c662Ceee821C',
      removeController: '0xBFB35262B8dF18829D33Da3A220DdE13505Cf485',
    },
    {
      publicKey: '0xda4f31b2d26acac4143d67f6b0ebc5a7b9fa829e4b63970f28130bff361316c9',
      activateController: '0x9c4a7E4964d0b14c3B436b522F0BFA33DD517350',
      removeController: '0xF75431B62AECD7da6AF24b6c0dFEd419579760c8',
    },
    {
      publicKey: '0x7bd0ca04c9423d39c25ee7747d2a5977acf19ff9d7f835f8dc1a746c83e71070',
      activateController: '0x22C1Ba671ad7c01d00Afb8E159424A8568B6b3a7',
      removeController: '0x4e5bF0aBC4EFA3c94Bc1ed23b1dB4b40De43AafA',
    },
    {
      publicKey: '0xc3fbb251a140db946283233a3fe5762a05a8305e2a8eaefc41d5df2820e071fc',
      activateController: '0xD28Ae083CFEefd89cb2b0f6466351d4C5630Ac5F',
      removeController: '0x034BB1f6239a70f956d2b18054CF7Bf7Bbe77Cc4',
    },
    {
      publicKey: '0xdeb9620fb017834b90e4b0cf66fdc7fd06b3edb68f0147938e59a9fa7db83380',
      activateController: '0x5dD30B6C339ec59eB7fBeD6026C43978e5663FbE',
      removeController: '0x7dCCcD3f36dDc820265F6e5758044EA3d4739C80',
    },
    {
      publicKey: '0x254192a339b277911fae7dac3a52ed4b0196bb90670fb3b893f521849086fc8d',
      activateController: '0x4e8FC724848B3F119E031C1e17c5e68Af75D29C2',
      removeController: '0x5519855988085f55Ac7aCcD7aED1C53988D9aF18',
    },
  ]

  const config: GenesisConfig = {
    timestamp: BigInt(Math.floor(new Date('2026-05-12T00:00:00-00:00').getTime() / 1000)),

    coinbase: '0x3141592653589793238462643383279502884197',

    NativeFiatToken: {
      proxy: { admin: '0x9005E53E3ee2f27999F15e7a52C58f804Fc716e0' },
      owner: '0xf5a5658b55983E2Aa037cAC7A8431B510E8A97F4',
      pauser: zeroAddress,
      masterMinter: zeroAddress,
      blacklister: zeroAddress,
      rescuer: zeroAddress,
      minters: [],
    },

    ProtocolConfig: {
      proxy: { admin: '0xBD3738ab866eff9B0908Ef8985d00eECA22DA4eF' },
      owner: '0xA5FeD552f38E11291Dd2Fc9cb91e2a5F6Ae86eD4',
      controller: '0x41fE044f1f71ff69F46F35f41EC93369a0E94733',
      pauser: '0x85c8825829fC649694D569bcCc374E8382Df2D12',
      feeParams: {
        alpha: 20n,
        kRate: 200n,
        inverseElasticityMultiplier: 5000n,
        minBaseFee: parseGwei('20'),
        maxBaseFee: parseGwei('20000'),
        blockGasLimit: 30_000_000n,
      },
      consensusParams: {
        timeoutProposeMs: 3000n,
        timeoutProposeDeltaMs: 500n,
        timeoutPrevoteMs: 1000n,
        timeoutPrevoteDeltaMs: 500n,
        timeoutPrecommitMs: 1000n,
        timeoutPrecommitDeltaMs: 500n,
        timeoutRebroadcastMs: 5000n,
        targetBlockTimeMs: 500n,
      },
    },

    ValidatorManager: {
      proxy: { admin: '0x20Db45729BC366833107524804d16cEb44e946c6' },
      PermissionedValidatorManager: {
        proxy: { admin: '0x131E6B8E466aC8046c38c3ae7de77595CfEAf0D1' },
        owner: '0x20E61a9CC8d010928Aa9997e4e773e84dE1B8306',
        pauser: '0x5bC1d44F8e844863cbC45D2f425F4c3758faf7b8',
        validatorRegisterers: [
          '0xF65A7d2E6C16d263B7B263369deD8C78f3aa4813',
          '0x38c57d852eddE831CE368D127CDabf5AdB86Ce96',
        ],
      },
      // Each validator gets a pair of controllers: activate
      // (limit = initial voting power, can raise voting power) and remove
      // (limit = 0, can only zero it out).
      validators: validators.map((v) => ({
        publicKey: v.publicKey,
        votingPower: initialVotingPower,
        controllers: [
          { address: v.activateController, votingPowerLimit: initialVotingPower },
          { address: v.removeController, votingPowerLimit: 0n },
        ],
      })),
    },

    Denylist: {
      proxy: {
        address: '0x3600000000000000000000000000000000000004',
        admin: '0xAfb5b6a4725459959ef931a3e5df758a72A8cA7f',
      },
      owner: '0x49ec36db19623e4DaDc1Aa821CbA2D1476F8E859',
      denylisters: [],
    },

    prefund: [{ address: '0x50A2b0B577eC24d7ce1aeD372A8a6fd14CE1bE57', balance: parseEther('10000') }],
    hardforks: { osakaTime: 0 },
  }

  fs.writeFileSync(
    path.join(ctx.projectRoot, `assets/${ctx.network}/config.json`),
    JSON.stringify(config, bigintReplacer, 2) + '\n',
  )
  return await buildGenesis(ctx, config)
}

export default build
