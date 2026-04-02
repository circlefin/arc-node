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
import { expect } from 'chai'
import { createWalletClient, encodeDeployData, Hex, http, keccak256, parseAbi, parseGwei, toHex } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import {
  Denylist,
  DeterministicDeployerProxy,
  expectAddressEq,
  GasGuzzler,
  gasGuzzlerArtifact,
  getClients,
  ProtocolConfig,
} from '../helpers'
import { USDC } from '../helpers/FiatToken'
import { PermissionedValidatorManager, ValidatorRegistry, ValidatorStatus } from '../helpers/ValidatorManager'
import {
  memoAddress,
  denylistAddress,
  gasGuzzlerAddress,
  Manifest,
  multicall3Address,
  multicall3FromAddress,
} from '../../scripts/genesis'
import { getValidators } from '../helpers/networks/localdev'
import manifest from '../../assets/artifacts/manifest.json'

describe('genesis', () => {
  const clients = async () => {
    const { client, admin, proxyAdmin, operator, sender, getController } = await getClients()
    const protocolConfig = ProtocolConfig.attach(client).read
    const usdc = USDC.attach(client).read
    const validatorRegistry = ValidatorRegistry.attach(client).read
    const poaValidatorManager = PermissionedValidatorManager.attach(client).read
    const denylist = Denylist.attach(client).read
    return {
      client,
      protocolConfig,
      usdc,
      validatorRegistry,
      poaValidatorManager,
      denylist,
      getController,
      sender,
      expectAddr: {
        proxyAdmin: proxyAdmin.account.address,
        admin: admin.account.address,
        operator: operator.account.address,
      },
    }
  }

  it('chainId', async () => {
    const { client } = await getClients()
    const chainId = await client.getChainId()
    expect(chainId).to.equal(hre.network.config.chainId)
  })

  it('accounts', async () => {
    const { client } = await getClients()
    const accounts = await hre.viem.getWalletClients({ chain: client.chain })

    const results = await client.multicall({
      contracts: [
        ...accounts.map((account) => ({
          address: multicall3Address,
          abi: parseAbi(['function getEthBalance(address addr) external view returns (uint256 balance)']),
          functionName: 'getEthBalance',
          args: [account.account.address],
        })),
      ],
      multicallAddress: multicall3Address,
    })
    for (const res of results) {
      expect(res.status).to.equal('success')
      expect((res.result ?? 0n) > 0n).to.be.true
    }
  })

  it('account by private key', async () => {
    const { client } = await getClients()
    const account = createWalletClient({
      chain: client.chain,
      transport: http('url' in hre.network.config ? hre.network.config.url : undefined),
      account: privateKeyToAccount(toHex(1n, { size: 32 })),
    })
    const balance = await client.getBalance(account.account)
    expect(balance > 0n).to.be.true
  })

  it('deterministic deployer', async () => {
    const { client, sender } = await getClients()
    const callHelper = hre.artifacts.readArtifactSync('CallHelper')

    const callData = encodeDeployData({
      abi: callHelper.abi,
      bytecode: callHelper.bytecode as Hex,
      args: [],
    })
    const ktAddress = DeterministicDeployerProxy.getDeployAddress(callData)
    expect(ktAddress).to.addressEqual('0xb871ff5b9ae7f6e8d4e612428e626736cc2bacc5')

    const address = await DeterministicDeployerProxy.deployCode(sender, client, callData)
    expect(address).to.addressEqual(ktAddress)
  })

  describe('USDC contract setup', () => {
    it('implementation', async () => {
      const { client, usdc } = await clients()
      const impl = await usdc.implementation()
      const code = await client.getCode({ address: impl })
      expect(code?.length).to.greaterThan(0)
    })

    it('admin', async () => {
      const { usdc, expectAddr } = await clients()
      const [admin, owner, masterMinter, pauser, blacklister] = await Promise.all([
        usdc.admin(),
        usdc.owner(),
        usdc.masterMinter(),
        usdc.pauser(),
        usdc.blacklister(),
      ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner).to.be.addressEqual(expectAddr.admin)
      expectAddressEq(masterMinter, expectAddr.admin)
      expectAddressEq(pauser, expectAddr.admin)
      expectAddressEq(blacklister, expectAddr.operator)
    })

    it('token info', async () => {
      const { usdc } = await clients()
      const [currency, symbol, name, decimals] = await Promise.all([
        usdc.currency(),
        usdc.symbol(),
        usdc.name(),
        usdc.decimals(),
      ])
      expect(currency, 'currency').to.be.eq('USD')
      expect(symbol, 'symbol').to.be.eq('USDC')
      expect(name, 'name').to.be.eq('USDC')
      expect(decimals, 'decimals').to.be.eq(6)
    })

    it('minter', async () => {
      const { usdc, expectAddr } = await clients()
      const minter = expectAddr.operator

      const [isMinter, minterAllowance] = await Promise.all([usdc.isMinter([minter]), usdc.minterAllowance([minter])])
      expect(isMinter, 'isMinter').to.be.true
      expect(minterAllowance > 0n, 'minterAllowance').to.be.true
    })
  })

  describe('protocol config', () => {
    it('initial addresses', async () => {
      const { protocolConfig, expectAddr } = await clients()

      const [admin, owner, controller, pauser, beneficiary] = await Promise.all([
        protocolConfig.admin(),
        protocolConfig.owner(),
        protocolConfig.controller(),
        protocolConfig.pauser(),
        protocolConfig.rewardBeneficiary(),
      ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(controller.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(pauser.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(beneficiary.toLowerCase()).to.be.eq(expectAddr.proxyAdmin)
    })

    it('fee params', async () => {
      const { protocolConfig } = await clients()
      const feeParams = await protocolConfig.feeParams()
      expect(feeParams.alpha).to.be.eq(20n)
      expect(feeParams.kRate).to.be.eq(200n)
      expect(feeParams.inverseElasticityMultiplier).to.be.eq(5000n)
      expect(feeParams.minBaseFee).to.be.eq(1n)
      expect(feeParams.maxBaseFee).to.be.eq(parseGwei('1000'))
      expect(feeParams.blockGasLimit).to.be.eq(30_000_000n)
    })
  })

  describe('validator registry', () => {
    it('initial addresses', async () => {
      const { validatorRegistry, expectAddr } = await clients()

      const [admin, owner] = await Promise.all([validatorRegistry.admin(), validatorRegistry.owner()])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(PermissionedValidatorManager.address)
    })

    it('get validator', async () => {
      const { validatorRegistry } = await clients()
      const validators = await getValidators()
      for (const validatorAccount of validators) {
        const validator = await validatorRegistry.getValidator([validatorAccount.registrationID])
        expect(validator.status).to.be.eq(ValidatorStatus.Active)
        expect(validator.publicKey).to.be.eq(validatorAccount.publicKey)
        expect(validator.votingPower).to.be.eq(validatorAccount.votingPower)
      }
    })

    it('get non-existent validator', async () => {
      const { validatorRegistry } = await clients()
      const validators = await getValidators()
      const validator = await validatorRegistry.getValidator([BigInt(validators.length + 1)])
      expect(validator.status).to.be.eq(0)
      expect(validator.publicKey).to.be.eq('0x')
      expect(validator.votingPower).to.be.eq(0n)
    })

    it('active validators', async () => {
      const { validatorRegistry } = await clients()
      const validators = await getValidators()
      const activeValidators = await validatorRegistry.getActiveValidatorSet()
      expect(activeValidators).to.have.lengthOf(validators.length)
      for (let i = 0; i < activeValidators.length; i++) {
        const validator = activeValidators[i]
        expect(validator.status).to.be.eq(ValidatorStatus.Active)
        expect(validator.publicKey).to.be.eq(validators[i].publicKey)
        expect(validator.votingPower).to.be.eq(validators[i].votingPower)
      }
    })

    it('active validators with positive voting power count', async () => {
      const { validatorRegistry } = await clients()
      const activeValidators = await validatorRegistry.getActiveValidatorSet()
      const expectedCount = activeValidators.reduce(
        (count, validator) => count + (validator.votingPower > 0n ? 1n : 0n),
        0n,
      )
      const count = await validatorRegistry.getActiveValidatorsWithPositiveVotingPowerCount()
      expect(count).to.be.eq(expectedCount)
    })
  })

  describe('permissioned validator manager', () => {
    it('initial addresses', async () => {
      const { poaValidatorManager, expectAddr, getController } = await clients()
      const controller1 = getController(1n)
      const controller5 = getController(5n)

      const [admin, owner, isController1, isController5, isValidatorRegisterer1, isValidatorRegisterer2] =
        await Promise.all([
          poaValidatorManager.admin(),
          poaValidatorManager.owner(),
          poaValidatorManager.isController([controller1.account.address]),
          poaValidatorManager.isController([controller5.account.address]),
          poaValidatorManager.isValidatorRegisterer([expectAddr.admin]),
          poaValidatorManager.isValidatorRegisterer([expectAddr.operator]),
        ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(isController1).to.be.true
      expect(isController5).to.be.true
      expect(isValidatorRegisterer1).to.be.true
      expect(isValidatorRegisterer2).to.be.true
    })
  })

  describe('denylist', () => {
    it('contract deployed at deterministic address', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: Denylist.address })
      expect(code?.length).to.be.greaterThan(0)
      expect(Denylist.address).to.be.addressEqual(denylistAddress)
    })

    it('implementation contract exists', async () => {
      const { client, denylist } = await clients()
      const impl = await denylist.implementation()
      const code = await client.getCode({ address: impl })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('initial addresses', async () => {
      const { denylist, expectAddr } = await clients()
      const [admin, owner] = await Promise.all([denylist.admin(), denylist.owner()])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(expectAddr.admin)
    })

    it('operator is initial denylister in localdev', async () => {
      const { denylist, sender, expectAddr } = await clients()
      const [isOperatorDenylister, isSenderDenylister] = await Promise.all([
        denylist.isDenylister([expectAddr.operator]),
        denylist.isDenylister([sender.account.address]),
      ])
      expect(isOperatorDenylister).to.be.true
      expect(isSenderDenylister).to.be.false
    })

    it('no addresses denylisted in genesis', async () => {
      const { denylist, expectAddr } = await clients()
      const [isAdminDenylisted, isOperatorDenylisted] = await Promise.all([
        denylist.isDenylisted([expectAddr.admin]),
        denylist.isDenylisted([expectAddr.operator]),
      ])
      expect(isAdminDenylisted).to.be.false
      expect(isOperatorDenylisted).to.be.false
    })

    it('storage slot matches ERC-7201 formula', async () => {
      const { client } = await getClients()
      // ERC-7201: keccak256(abi.encode(uint256(keccak256("arc.storage.Denylist.v1")) - 1)) & ~bytes32(uint256(0xff))
      const namespace = 'arc.storage.Denylist.v1'
      const namespaceHash = BigInt(keccak256(toHex(namespace)))
      const preImage = (namespaceHash - 1n).toString(16).padStart(64, '0')
      const storageLocationHash = keccak256(`0x${preImage}`)
      const storageLocation = BigInt(storageLocationHash) & ~BigInt(0xff)

      const expectedSlot = '0x1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00'
      expect(`0x${storageLocation.toString(16)}`).to.be.eq(expectedSlot)

      // Verify contract constant matches
      const denylistContract = Denylist.attach(client)
      const contractStorageLocation = await denylistContract.read.DENYLIST_STORAGE_LOCATION()
      expect(contractStorageLocation).to.be.eq(expectedSlot)
    })
  })

  describe('GasGuzzler', () => {
    it('deployed at expected address', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: gasGuzzlerAddress })
      expect(code?.length).to.greaterThan(0)
    })

    it('bytecode matches artifact', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: gasGuzzlerAddress })
      expect(code).to.equal(gasGuzzlerArtifact.deployedBytecode)
    })

    it('hashLoop is callable', async () => {
      const { client } = await getClients()
      const guzzler = GasGuzzler.attach(client, gasGuzzlerAddress)
      const result = await guzzler.read.hashLoop([10n])
      expect(result).to.be.a('string')
      expect(result).to.have.length(66) // bytes32 = 0x + 64 hex chars
    })
  })

  describe('Memo', () => {
    it('deployed at expected address', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: memoAddress })
      expect(code?.length).to.greaterThan(0)
    })

    it('bytecode matches artifact', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: memoAddress })
      const artifact = hre.artifacts.readArtifactSync('Memo')
      expect(code).to.equal(artifact.deployedBytecode)
    })
  })

  describe('Multicall3From', () => {
    it('deployed at expected address', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: multicall3FromAddress })
      expect(code?.length).to.greaterThan(0)
    })

    it('bytecode matches artifact', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: multicall3FromAddress })
      const artifact = hre.artifacts.readArtifactSync('Multicall3From')
      expect(code).to.equal(artifact.deployedBytecode)
    })
  })

  describe('deployer nonce for one-time-address contracts', () => {
    const typedManifest = manifest as unknown as Manifest
    const oneTimeAddressEntries = Object.entries(typedManifest).filter(([, entry]) => entry.type === 'one-time-address')
    const deterministicEntries = Object.entries(typedManifest).filter(([, entry]) => entry.type === 'deterministic')

    for (const [contractName, entry] of oneTimeAddressEntries) {
      if (entry.type !== 'one-time-address') continue

      it(`${contractName} deployer (${entry.deployer}) has nonce=1`, async () => {
        const { client } = await getClients()
        const nonce = await client.getTransactionCount({ address: entry.deployer })
        expect(nonce).to.equal(1)
      })

      it(`${contractName} deployer (${entry.deployer}) has balance=0`, async () => {
        const { client } = await getClients()
        const balance = await client.getBalance({ address: entry.deployer })
        expect(balance).to.equal(0n)
      })
    }

    for (const [contractName, entry] of deterministicEntries) {
      it(`${contractName} (deterministic) does not produce a deployer alloc`, async () => {
        const { client } = await getClients()
        // Deterministic contracts use CREATE2 via the DeterministicDeploymentProxy,
        // so there is no separate deployer address to initialize.
        const nonce = await client.getTransactionCount({ address: entry.address })
        expect(nonce).to.equal(1, 'contract itself should have nonce=1')
        // Verify no "deployer" field exists on deterministic entries
        expect('deployer' in entry).to.be.false
      })
    }
  })
})
