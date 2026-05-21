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
import hre from 'hardhat'
import { expect } from 'chai'
import {
  Address,
  concat,
  createWalletClient,
  encodeAbiParameters,
  encodeDeployData,
  encodeFunctionData,
  Hex,
  http,
  keccak256,
  parseAbi,
  parseGwei,
  toHex,
} from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import {
  AdminUpgradeableProxy,
  Denylist,
  DeterministicDeployerProxy,
  expectAddressEq,
  GasGuzzler,
  gasGuzzlerArtifact,
  getClients,
  ProtocolConfig,
  readForgeArtifactSync,
} from '../helpers'
import { USDC } from '../helpers/FiatToken'
import { PermissionedValidatorManager, ValidatorRegistry, ValidatorStatus } from '../helpers/ValidatorManager'
import {
  memoAddress,
  denylistAddressByNetwork,
  gasGuzzlerAddress,
  Manifest,
  multicall3Address,
  multicall3FromAddress,
  permissionedManagerAddress,
  protocolConfigAddress,
  validatorRegistryAddress,
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

  // Regression guards: compute the CREATE2 address from current Forge-compiled bytecode
  // (what genesis deploys), then assert it matches BOTH the hardcoded constant in
  // scripts/genesis/addresses.ts AND the genesis placement (code present at that address
  // on-chain). Guards against:
  //   - stale constants when bytecode shifts (compiler settings, source edits)
  //   - stale genesis when constants shift but genesis wasn't regenerated
  describe('CREATE2 reproducibility', () => {
    // Helper: read the implementation slot of an AdminUpgradeableProxy at `proxyAddress`.
    // Used to verify proxies point at the CREATE2 impl address we compute from bytecode.
    const implAt = async (proxyAddress: Address): Promise<Address> => {
      const { client } = await getClients()
      return AdminUpgradeableProxy.attach(client, proxyAddress).read.implementation()
    }

    // The stablecoin contracts (SignatureChecker, NativeFiatTokenV2_2, FiatTokenProxy) are
    // not compiled locally — they ship as static artifacts under
    // assets/artifacts/stablecoin-contracts/. Read those directly for CREATE2 recomputation.
    const loadStablecoinArtifact = (name: string) => {
      const p = path.join(__dirname, '../../assets/artifacts/stablecoin-contracts', `${name}.json`)
      return JSON.parse(fs.readFileSync(p, 'utf8')) as { bytecode: string; linkReferences?: unknown }
    }

    it('Memo (genesis-placed)', async () => {
      const { client } = await getClients()
      const memoArtifact = readForgeArtifactSync('Memo')
      const computed = DeterministicDeployerProxy.getDeployAddress(memoArtifact.bytecode)

      // (1) computed address matches hardcoded constant
      expect(computed).to.be.addressEqual(memoAddress)

      // (2) genesis placed code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('Multicall3From (genesis-placed)', async () => {
      const { client } = await getClients()
      const m3fArtifact = readForgeArtifactSync('Multicall3From')
      const computed = DeterministicDeployerProxy.getDeployAddress(m3fArtifact.bytecode)

      // (1) computed address matches hardcoded constant
      expect(computed).to.be.addressEqual(multicall3FromAddress)

      // (2) genesis placed code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('ProtocolConfig implementation (salt=0)', async () => {
      const { client } = await getClients()
      const artifact = readForgeArtifactSync('ProtocolConfig')
      const computed = DeterministicDeployerProxy.getDeployAddress(artifact.bytecode)

      // (1) on-chain proxy's IMPL_SLOT points at the CREATE2 address
      expect(await implAt(protocolConfigAddress)).to.be.addressEqual(computed)

      // (2) genesis placed code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('ValidatorRegistry implementation (salt=0)', async () => {
      const { client } = await getClients()
      const artifact = readForgeArtifactSync('ValidatorRegistry')
      const computed = DeterministicDeployerProxy.getDeployAddress(artifact.bytecode)

      expect(await implAt(validatorRegistryAddress)).to.be.addressEqual(computed)

      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('PermissionedValidatorManager implementation (salt=0, ctor arg: validatorRegistryProxy)', async () => {
      const { client } = await getClients()
      const artifact = readForgeArtifactSync('PermissionedValidatorManager')
      const ctorArgs = encodeAbiParameters([{ type: 'address' }], [validatorRegistryAddress])
      const fullInit = concat([artifact.bytecode, ctorArgs])
      const computed = DeterministicDeployerProxy.getDeployAddress(fullInit)

      expect(await implAt(permissionedManagerAddress)).to.be.addressEqual(computed)

      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    it('NativeFiatTokenV2_2 implementation (salt=0, linked with SignatureChecker)', async () => {
      const { client, usdc } = await clients()

      // 1. SignatureChecker CREATE2 (salt=0, no args) from static stablecoin artifact
      const sc = loadStablecoinArtifact('SignatureChecker')
      const scAddress = DeterministicDeployerProxy.getDeployAddress(sc.bytecode as Hex)

      // 2. NativeFiatTokenV2_2 has a library placeholder for SignatureChecker; replace with
      //    the computed address before hashing. Placeholder format: __$<34-hex-hash>$__.
      const nft = loadStablecoinArtifact('NativeFiatTokenV2_2')
      const placeholder = '__$715109b5d747ea58b675c6ea3f0dba8c60$__'
      const linked = nft.bytecode.split(placeholder).join(scAddress.slice(2).toLowerCase())

      const computed = DeterministicDeployerProxy.getDeployAddress(linked as Hex)

      // (1) FiatTokenProxy's implementation points at the computed address
      expect(await usdc.implementation()).to.be.addressEqual(computed)

      // (2) genesis placed code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })
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
      const [admin, owner, masterMinter, pauser, blacklister, rescuer] = await Promise.all([
        usdc.admin(),
        usdc.owner(),
        usdc.masterMinter(),
        usdc.pauser(),
        usdc.blacklister(),
        usdc.rescuer(),
      ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner).to.be.addressEqual(expectAddr.admin)
      expectAddressEq(masterMinter, expectAddr.admin)
      expectAddressEq(pauser, expectAddr.admin)
      expectAddressEq(blacklister, expectAddr.operator)
      expectAddressEq(rescuer, expectAddr.admin)
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

      const [admin, owner, controller, pauser] = await Promise.all([
        protocolConfig.admin(),
        protocolConfig.owner(),
        protocolConfig.controller(),
        protocolConfig.pauser(),
      ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(controller.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(pauser.toLowerCase()).to.be.eq(expectAddr.admin)
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

    // Guards the bit-packed uint16 layout in ProtocolConfig.ts: a mis-shifted
    // field would silently ship wrong consensus timeouts to malachite.
    it('consensus params', async () => {
      const { protocolConfig } = await clients()
      const consensusParams = await protocolConfig.consensusParams()
      expect(consensusParams.timeoutProposeMs).to.be.eq(3000)
      expect(consensusParams.timeoutProposeDeltaMs).to.be.eq(500)
      expect(consensusParams.timeoutPrevoteMs).to.be.eq(1000)
      expect(consensusParams.timeoutPrevoteDeltaMs).to.be.eq(500)
      expect(consensusParams.timeoutPrecommitMs).to.be.eq(1000)
      expect(consensusParams.timeoutPrecommitDeltaMs).to.be.eq(500)
      expect(consensusParams.timeoutRebroadcastMs).to.be.eq(1000)
      expect(consensusParams.targetBlockTimeMs).to.be.eq(500)
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
      const { poaValidatorManager, expectAddr } = await clients()

      const [admin, owner, pauser, isValidatorRegisterer1, isValidatorRegisterer2] = await Promise.all([
        poaValidatorManager.admin(),
        poaValidatorManager.owner(),
        poaValidatorManager.pauser(),
        poaValidatorManager.isValidatorRegisterer([expectAddr.admin]),
        poaValidatorManager.isValidatorRegisterer([expectAddr.operator]),
      ])
      expect(admin).to.be.addressEqual(expectAddr.proxyAdmin)
      expect(owner.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(pauser.toLowerCase()).to.be.eq(expectAddr.admin)
      expect(isValidatorRegisterer1).to.be.true
      expect(isValidatorRegisterer2).to.be.true
    })

    // Every validator's controller must be wired at genesis to its registrationId
    // with the configured voting power limit (UINT64_MAX in localdev). Guards
    // against regressions in slotForAddressMap or per-controller flattening.
    it('controllers wired with voting power limits', async () => {
      const { poaValidatorManager, getController } = await clients()
      const validators = await getValidators()
      const UINT64_MAX = (1n << 64n) - 1n

      await Promise.all(
        validators.map(async (v) => {
          const controller = getController(v.registrationID)
          const [isCtrl, limit] = await Promise.all([
            poaValidatorManager.isController([controller.account.address]),
            poaValidatorManager.getVotingPowerLimit([controller.account.address]),
          ])
          expect(isCtrl, `controller ${v.registrationID} registered`).to.be.true
          expect(limit, `controller ${v.registrationID} voting power limit`).to.be.eq(UINT64_MAX)
        }),
      )
    })
  })

  describe('denylist', () => {
    it('contract deployed at deterministic address', async () => {
      const { client } = await getClients()
      const code = await client.getCode({ address: Denylist.address })
      expect(code?.length).to.be.greaterThan(0)
      expect(Denylist.address).to.be.addressEqual(denylistAddressByNetwork.localdev)
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

    // Regression guard: compute the Denylist implementation CREATE2 address (salt=0) from
    // current Forge bytecode, and assert it matches BOTH the on-chain proxy's IMPL_SLOT AND that
    // runtime code is present at that address. Catches drift if bytecode changes without
    // genesis regeneration.
    it('implementation at expected CREATE2 address (salt=0)', async () => {
      const { client, denylist } = await clients()
      const denylistArtifact = readForgeArtifactSync('Denylist')
      const computed = DeterministicDeployerProxy.getDeployAddress(denylistArtifact.bytecode)

      // (1) computed matches on-chain IMPL_SLOT (proxy points at genesis-placed impl)
      const onChainImpl = await denylist.implementation()
      expect(onChainImpl).to.be.addressEqual(computed)

      // (2) genesis placed runtime code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
    })

    // Regression guard: compute the Denylist proxy CREATE2 address from
    //   AdminUpgradeableProxy bytecode + abi.encode(impl, proxyAdmin, initData)
    // combined with the documented mined salt, and assert it matches BOTH the hardcoded
    // denylistAddressByNetwork.localdev constant AND that runtime code is placed at that
    // address in genesis. Mined via `INIT_CODE_HASH=<hash> make mine-denylist-salt` —
    // see scripts/genesis/addresses.ts.
    it('proxy at expected CREATE2 address (mined salt)', async () => {
      const { client, denylist } = await clients()
      const denylistArtifact = readForgeArtifactSync('Denylist')
      const proxyArtifact = readForgeArtifactSync('AdminUpgradeableProxy')

      const impl = DeterministicDeployerProxy.getDeployAddress(denylistArtifact.bytecode)
      const [owner, proxyAdmin] = await Promise.all([denylist.owner(), denylist.admin()])

      const initData = encodeFunctionData({
        abi: Denylist.abi,
        functionName: 'initialize',
        args: [owner],
      })
      const ctorArgs = encodeAbiParameters(
        [{ type: 'address' }, { type: 'address' }, { type: 'bytes' }],
        [impl, proxyAdmin, initData],
      )
      const fullInit = concat([proxyArtifact.bytecode, ctorArgs])

      const MINED_SALT = 0x2e8184e0b708cc70e9f829091612c4c8efef8006ee7527c777e0bd70b64c36c8n
      const computed = DeterministicDeployerProxy.getDeployAddress(fullInit, MINED_SALT)

      // (1) computed matches hardcoded constant
      expect(computed).to.be.addressEqual(denylistAddressByNetwork.localdev)

      // (2) genesis placed runtime code at the computed address
      const code = await client.getCode({ address: computed })
      expect(code?.length).to.be.greaterThan(0)
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
      const artifact = readForgeArtifactSync('Memo')
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
      const artifact = readForgeArtifactSync('Multicall3From')
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
