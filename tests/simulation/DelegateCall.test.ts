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
import { USDC } from '../helpers/FiatToken'
import { pad, encodeFunctionData, parseEther, Address } from 'viem'
import { Hex } from 'viem'
import { getChain } from '../../scripts/hardhat/viem-helper'
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts'
import { fiatTokenProxyAddress, nativeCoinAutorityAddress } from '../../scripts/genesis/addresses'

/**
 * Delegatecall Attack Tests using Simulation with State Overrides
 */
describe('Delegatecall Attack (Simulation)', () => {
  // Helper: Generate random address
  const randomAddress = () => privateKeyToAccount(generatePrivateKey()).address

  // Helper: Get malicious contract artifact
  const getMaliciousContract = async () => {
    return await hre.artifacts.readArtifact('PrecompileDelegater')
  }

  // Helper: Create state override for malicious contract
  const createStateOverride = (address: Address, code: Hex) => [{ address, code }]

  it('should prevent ERC1271 permit attack via staticcall', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const usdc = USDC.attach(client)

    const maliciousSignerAddress = randomAddress()
    const deployerAddress = randomAddress()
    const artifact = await getMaliciousContract()
    const stateOverride = createStateOverride(maliciousSignerAddress, artifact.deployedBytecode as Hex)

    // Read the owner from the simulated contract
    const signerOwner = await client.readContract({
      address: maliciousSignerAddress,
      abi: artifact.abi,
      functionName: 'owner',
      args: [],
      stateOverride,
    })

    // Check initial balances
    const initialOwnerUSDCBalance = await usdc.read.balanceOf([signerOwner])
    const initialTotalSupply = await usdc.read.totalSupply()

    // Prepare permit parameters with random spender
    const deadline = 2n ** 256n - 1n // Max uint256 - never expires
    const dummySignature = pad('0x00', { size: 65 })

    // Simulate a block with permit call and balance checks, using state override for malicious contract
    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              from: deployerAddress,
              to: USDC.address,
              data: encodeFunctionData({
                abi: usdc.abi,
                functionName: 'permit',
                args: [maliciousSignerAddress, randomAddress(), 1000000n, deadline, dummySignature],
              }),
            },
            {
              from: deployerAddress,
              to: USDC.address,
              data: encodeFunctionData({
                abi: usdc.abi,
                functionName: 'balanceOf',
                args: [signerOwner],
              }),
            },
            {
              from: deployerAddress,
              to: USDC.address,
              data: encodeFunctionData({
                abi: usdc.abi,
                functionName: 'totalSupply',
                args: [],
              }),
            },
          ],
          stateOverrides: stateOverride,
        },
      ],
    })

    const calls = blockSimulation[0].calls

    expect(calls).to.have.lengthOf(3)
    expect(calls[0].status).to.equal('failure', 'Permit call should fail')
    // Assert failure reason contains expected error message
    expect(calls[0].error?.message).to.include('execution reverted: EIP2612: invalid signature')
    expect(calls[1].status).to.equal('success', 'Balance check should succeed')
    expect(calls[2].status).to.equal('success', 'Total supply check should succeed')

    // Decode the balance results from the simulated block
    const finalOwnerUSDCBalance = BigInt(calls[1].data)
    const finalTotalSupply = BigInt(calls[2].data)

    const usdcBalanceIncrease = finalOwnerUSDCBalance - initialOwnerUSDCBalance
    const supplyIncrease = finalTotalSupply - initialTotalSupply

    // Assert that no minting occurred
    expect(usdcBalanceIncrease).to.equal(0n, 'No balance change expected')
    expect(supplyIncrease).to.equal(0n, 'No supply change expected')
  })

  it('should prevent rescueERC20 attack via delegatecall blocking', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const usdc = USDC.attach(client)

    const rescuerAddress = await usdc.read.rescuer()
    const maliciousContractAddress = randomAddress()
    const artifact = await getMaliciousContract()
    const stateOverride = createStateOverride(maliciousContractAddress, artifact.deployedBytecode as Hex)

    const rescueCalldata = encodeFunctionData({
      abi: usdc.abi,
      functionName: 'rescueERC20',
      args: [maliciousContractAddress, randomAddress(), parseEther('98765')],
    })

    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [{ from: rescuerAddress, to: USDC.address, data: rescueCalldata }],
          stateOverrides: stateOverride,
        },
      ],
    })

    const calls = blockSimulation[0].calls

    expect(calls).to.have.lengthOf(1)
    expect(calls[0].status).to.equal('failure', 'rescueERC20 should fail (delegatecall blocked)')
    expect(calls[0].error).to.exist
    // Assert failure reason contains expected error message
    expect(calls[0].error?.message).to.include('execution reverted: Delegate call not allowed')
  })

  it('regular call to NativeCoinAuthority.mint should increase USDC balance in simulation', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const usdc = USDC.attach(client)

    const recipient = randomAddress()
    const mintAmount = parseEther('100')
    const expectedUSDCIncrease = mintAmount / 10n ** 12n // Convert 18 decimal to 6 decimal

    // Encode mint call
    const mintCalldata = encodeFunctionData({
      abi: [{ type: 'function', name: 'mint', inputs: [{ type: 'address' }, { type: 'uint256' }] }],
      functionName: 'mint',
      args: [recipient, mintAmount],
    })

    // Encode USDC balance check
    const balanceCheckCalldata = encodeFunctionData({
      abi: usdc.abi,
      functionName: 'balanceOf',
      args: [recipient],
    })

    // Simulate: Direct call to NativeCoinAuthority.mint from the FiatToken proxy,
    // which is the hardcoded allowed caller under Zero5+.
    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              from: fiatTokenProxyAddress,
              to: nativeCoinAutorityAddress,
              data: mintCalldata,
            },
            {
              from: randomAddress(),
              to: USDC.address,
              data: balanceCheckCalldata,
            },
          ],
        },
      ],
    })

    const calls = blockSimulation[0].calls

    expect(calls).to.have.lengthOf(2)
    expect(calls[0].status).to.equal('success', 'Mint should succeed with authorized caller')
    expect(calls[1].status).to.equal('success', 'Balance check should succeed')

    // Decode USDC balance - should show the minted amount within simulation
    const usdcBalance = BigInt(calls[1].data)
    expect(usdcBalance).to.equal(expectedUSDCIncrease, 'USDC balance should increase within simulation')
  })

  it('staticcall to NativeCoinAuthority.mint should NOT increase USDC balance', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const usdc = USDC.attach(client)

    const recipient = randomAddress()
    const mintAmount = parseEther('100')

    // Encode mint call
    const mintCalldata = encodeFunctionData({
      abi: [{ type: 'function', name: 'mint', inputs: [{ type: 'address' }, { type: 'uint256' }] }],
      functionName: 'mint',
      args: [recipient, mintAmount],
    })

    // Deploy CallHelper to make the staticcall
    const helperArtifact = await hre.artifacts.readArtifact('CallHelper')
    const helperAddress = randomAddress()
    const helperCode = helperArtifact.deployedBytecode as Hex

    // Encode CallHelper.staticCall to NativeCoinAuthority.mint
    const staticCallData = encodeFunctionData({
      abi: helperArtifact.abi,
      functionName: 'staticCall',
      args: [nativeCoinAutorityAddress, mintCalldata],
    })

    // Encode USDC balance check
    const balanceCheckCalldata = encodeFunctionData({
      abi: usdc.abi,
      functionName: 'balanceOf',
      args: [recipient],
    })

    // Simulate: CallHelper.staticCall -> NativeCoinAuthority.mint, then check USDC balance
    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              from: randomAddress(),
              to: helperAddress,
              data: staticCallData,
            },
            {
              from: randomAddress(),
              to: USDC.address,
              data: balanceCheckCalldata,
            },
          ],
          stateOverrides: [
            {
              address: helperAddress,
              code: helperCode,
            },
            {
              address: nativeCoinAutorityAddress,
              stateDiff: [
                {
                  slot: pad('0x1', { size: 32 }), // slot 1 = fiatTokenAddress
                  value: pad(helperAddress, { size: 32 }), // Authorize CallHelper
                },
              ],
            },
          ],
        },
      ],
    })

    const calls = blockSimulation[0].calls

    expect(calls).to.have.lengthOf(2)
    expect(calls[0].status).to.equal('success', 'staticCall should complete')
    expect(calls[1].status).to.equal('success', 'Balance check should succeed')

    // Decode USDC balance - should be 0 because staticcall blocked the mint
    const usdcBalance = BigInt(calls[1].data)
    expect(usdcBalance).to.equal(0n, 'USDC balance should remain 0 - staticcall blocked mint')
  })

  it('staticcall -> delegatecall -> mint should NOT increase USDC balance', async () => {
    const client = await hre.viem.getPublicClient({ chain: getChain(hre) })
    const usdc = USDC.attach(client)

    const recipient = randomAddress()
    const mintAmount = parseEther('100')

    // Encode mint call
    const mintCalldata = encodeFunctionData({
      abi: [{ type: 'function', name: 'mint', inputs: [{ type: 'address' }, { type: 'uint256' }] }],
      functionName: 'mint',
      args: [recipient, mintAmount],
    })

    // Deploy two CallHelpers: A (for staticcall) and B (for delegatecall)
    const helperArtifact = await hre.artifacts.readArtifact('CallHelper')
    const helperA = randomAddress()
    const helperB = randomAddress()
    const helperCode = helperArtifact.deployedBytecode as Hex

    // Encode CallHelper B's delegateCall to NativeCoinAuthority.mint
    const delegateCallData = encodeFunctionData({
      abi: helperArtifact.abi,
      functionName: 'delegateCall',
      args: [nativeCoinAutorityAddress, mintCalldata],
    })

    // Encode CallHelper A's staticCall to CallHelper B's delegateCall
    const staticCallData = encodeFunctionData({
      abi: helperArtifact.abi,
      functionName: 'staticCall',
      args: [helperB, delegateCallData],
    })

    // Encode USDC balance check
    const balanceCheckCalldata = encodeFunctionData({
      abi: usdc.abi,
      functionName: 'balanceOf',
      args: [recipient],
    })

    // Simulate: CallHelper A.staticCall -> CallHelper B.delegateCall -> NativeCoinAuthority.mint
    const blockSimulation = await client.simulateBlocks({
      blocks: [
        {
          calls: [
            {
              from: randomAddress(),
              to: helperA,
              data: staticCallData,
            },
            {
              from: randomAddress(),
              to: USDC.address,
              data: balanceCheckCalldata,
            },
          ],
          stateOverrides: [
            {
              address: helperA,
              code: helperCode,
            },
            {
              address: helperB,
              code: helperCode,
            },
            {
              address: nativeCoinAutorityAddress,
              stateDiff: [
                {
                  slot: pad('0x1', { size: 32 }), // slot 1 = fiatTokenAddress
                  value: pad(helperB, { size: 32 }), // Authorize CallHelper B
                },
              ],
            },
          ],
        },
      ],
    })

    const calls = blockSimulation[0].calls

    expect(calls).to.have.lengthOf(2)
    expect(calls[0].status).to.equal('success', 'staticCall->delegateCall should complete')
    expect(calls[1].status).to.equal('success', 'Balance check should succeed')

    // Decode USDC balance - should be 0 because staticcall blocked the mint even through delegatecall
    const usdcBalance = BigInt(calls[1].data)
    expect(usdcBalance).to.equal(0n, 'USDC balance should remain 0 - staticcall blocks mint even via delegatecall')
  })
})
