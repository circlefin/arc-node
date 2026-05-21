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
import { parseEther, type Hex } from 'viem'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { DeterministicDeployerProxy, getClients, ReceiptVerifier } from '../helpers'
import {
  callFromAddress,
  nativeCoinAutorityAddress,
  nativeCoinControlAddress,
  pqAddress,
  systemAccountingAddress,
} from '../../scripts/genesis/addresses'
import { LocalDevAccountCreator } from '../../scripts/genesis/AccountCreator'
import { createWalletClient } from '../../scripts/hardhat/viem-helper'

const PRECOMPILE_ADDRESSES: Record<string, Hex> = {
  NATIVE_COIN_AUTHORITY: nativeCoinAutorityAddress,
  NATIVE_COIN_CONTROL: nativeCoinControlAddress,
  SYSTEM_ACCOUNTING: systemAccountingAddress,
  CALL_FROM: callFromAddress,
  PQ: pqAddress,
  'reserved 0x..05': '0x1800000000000000000000000000000000000005',
  'reserved 0x..ff': '0x18000000000000000000000000000000000000ff',
}

const localSigningSender = () => {
  const ac = new LocalDevAccountCreator()
  const { sender } = ac.namedAccounts(ac.defaultAccounts())
  return createWalletClient(hre, sender)
}

describe('Precompile bytecode (0xef)', () => {
  it('eth_getCode returns 0xef for active precompiles and sampled reserved slots', async () => {
    const { client } = await getClients()
    for (const [name, address] of Object.entries(PRECOMPILE_ADDRESSES)) {
      const code = await client.getCode({ address })
      expect(code, name).to.equal('0xef')
    }
  })

  // 7702 delegation bypasses precompile dispatch and executes the raw 0xef byte
  // → EIP-3541 reserved opcode → OpcodeNotFound, which Reth surfaces during
  // eth_estimateGas before the tx ever reaches the mempool.
  it('EIP-7702 delegation to an active precompile is rejected with OpcodeNotFound', async () => {
    const { client } = await getClients()
    const sender = localSigningSender()
    const ephemeral = createWalletClient(hre, privateKeyToAccount(generatePrivateKey()))
    await sender
      .sendTransaction({ to: ephemeral.account.address, value: parseEther('0.1') })
      .then(ReceiptVerifier.waitSuccess)

    const authorization = await ephemeral.signAuthorization({
      account: ephemeral.account,
      contractAddress: nativeCoinAutorityAddress,
    })

    await expect(
      sender.sendTransaction({
        to: ephemeral.account.address,
        value: 0n,
        authorizationList: [authorization],
      }),
    ).to.be.rejectedWith(/OpcodeNotFound/)

    const codeAfter = await client.getCode({ address: ephemeral.account.address })
    expect(codeAfter).to.be.undefined
  })

  // Same delegation as above but with an explicit `gas` field — viem skips
  // eth_estimateGas, so the tx reaches the mempool and is included in a block.
  // The 7702 authorization is applied (EOA gets the 0xef0100||target indicator),
  // but the call into the EOA resolves the delegation, executes the 0xef byte
  // at the precompile, reverts, and burns all supplied gas (the hallmark of an
  // invalid opcode).
  it('EIP-7702 delegation reverts on-chain when estimation is bypassed', async () => {
    const { client } = await getClients()
    const sender = localSigningSender()
    const ephemeral = createWalletClient(hre, privateKeyToAccount(generatePrivateKey()))

    const authorization = await ephemeral.signAuthorization({
      account: ephemeral.account,
      contractAddress: nativeCoinAutorityAddress,
    })

    const gas = 200_000n
    const hash = await sender.sendTransaction({
      to: ephemeral.account.address,
      value: 0n,
      authorizationList: [authorization],
      gas,
    })
    const receipt = await client.waitForTransactionReceipt({ hash })
    expect(receipt.status).to.equal('reverted')
    expect(receipt.gasUsed, 'invalid opcode burns all supplied gas').to.equal(gas)

    const codeAfter = await client.getCode({ address: ephemeral.account.address })
    expect(codeAfter?.toLowerCase()).to.equal(`0xef0100${nativeCoinAutorityAddress.slice(2).toLowerCase()}`)
  })

  // On Arc, USDC is the native gas token, so a plain value transfer with no
  // calldata to a reserved precompile slot (0xef code, no dispatcher) executes
  // the EIP-3541 reserved byte and traps with OpcodeNotFound. The estimation
  // path is rejected at submission; the explicit-gas path lands on-chain,
  // reverts, and burns all supplied gas (the hallmark of an invalid opcode).
  it('native USDC transfer to a reserved precompile slot fails with OpcodeNotFound', async () => {
    const { client, sender } = await getClients()
    const reserved: Hex = '0x1800000000000000000000000000000000000005'

    await expect(sender.sendTransaction({ to: reserved, value: 1n })).to.be.rejectedWith(/OpcodeNotFound/)

    const gas = 100_000n
    const hash = await sender.sendTransaction({ to: reserved, value: 1n, gas })
    const receipt = await client.waitForTransactionReceipt({ hash })
    expect(receipt.status).to.equal('reverted')
    expect(receipt.gasUsed, 'invalid opcode burns all supplied gas').to.equal(gas)
  })

  // Targeting a real precompile address via CREATE2 isn't feasible, so predict
  // a CREATE2 address via the deterministic deployer, then `stateOverrides`
  // 0xef into its code. Pattern mirrors tests/simulation/DelegateCall.test.ts.
  it('CREATE2 to a 0xef-coded address triggers CreateCollision', async () => {
    const { client, sender } = await getClients()
    const initcode: Hex = '0x5f5ff3'
    const salt = 0xefef_efef_efef_efefn
    const target = DeterministicDeployerProxy.getDeployAddress(initcode, salt)
    const data = DeterministicDeployerProxy.getDeployData(initcode, salt)

    const baseline = await client.simulateCalls({
      account: sender.account.address,
      calls: [{ to: DeterministicDeployerProxy.address, data }],
    })
    expect(baseline.results[0].status).to.equal('success')
    expect(baseline.results[0].data.toLowerCase()).to.include(target.slice(2).toLowerCase())

    const collision = await client.simulateCalls({
      account: sender.account.address,
      calls: [{ to: DeterministicDeployerProxy.address, data }],
      stateOverrides: [{ address: target, code: '0xef' }],
    })
    const result = collision.results[0]
    expect(result.status).to.equal('failure')
    const reason = (result as { error?: { cause?: { reason?: string } } }).error?.cause?.reason
    expect(reason).to.equal('execution reverted')
  })
})
