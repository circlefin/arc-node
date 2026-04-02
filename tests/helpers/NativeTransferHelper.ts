// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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
import { Address, encodeAbiParameters, TransactionReceipt, encodeFunctionData, Hex, zeroAddress } from 'viem'
import { PublicClient, WalletClient } from '@nomicfoundation/hardhat-viem/types'
import { ArtifactsMap } from 'hardhat/types'

export class NativeTransferHelper {
  public readonly address: Address
  public readonly abi: ArtifactsMap['NativeTransferHelper']['abi']
  public readonly bytecode: Hex
  public readonly client: PublicClient
  public readonly deploymentReceipt: TransactionReceipt

  private constructor(
    address: Address,
    abi: ArtifactsMap['NativeTransferHelper']['abi'],
    bytecode: Hex,
    client: PublicClient,
    deploymentReceipt: TransactionReceipt,
  ) {
    this.address = address
    this.abi = abi
    this.bytecode = bytecode
    this.client = client
    this.deploymentReceipt = deploymentReceipt
  }

  static async deploy(account: WalletClient, client: PublicClient, value: bigint, selfDestructTarget?: Address) {
    const artifact = await hre.artifacts.readArtifact('NativeTransferHelper')
    const deployedTxHash = await account.deployContract({
      abi: artifact.abi,
      bytecode: artifact.bytecode,
      args: [selfDestructTarget ?? zeroAddress, selfDestructTarget !== undefined] as const,
      value,
    })
    const receipt: TransactionReceipt = await client.waitForTransactionReceipt({ hash: deployedTxHash })
    if (!receipt.contractAddress) {
      throw new Error('Deployment failed, missing contract address')
    }
    return new NativeTransferHelper(receipt.contractAddress, artifact.abi, artifact.bytecode as Hex, client, receipt)
  }

  async callCanReceive(account: WalletClient, value: bigint): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'canReceive',
      args: [],
      value,
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  async callRelay(
    account: WalletClient,
    destination: Address,
    callerValue: bigint,
    relayValue: bigint,
    requireSuccess: boolean,
    callData: Hex,
    gas?: bigint,
  ): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'relay',
      value: callerValue,
      args: [destination, relayValue, requireSuccess, callData],
      gas,
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  async callCreate(
    account: WalletClient,
    bytecode: Hex,
    value: bigint,
    createValue: bigint,
  ): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'create',
      value,
      args: [bytecode, createValue],
      gas: 2000000n,
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  async callCreate2(account: WalletClient, bytecode: Hex, salt: Hex, value: bigint): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'create2',
      value: value,
      args: [bytecode, salt],
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  async callSelfDestruct(account: WalletClient, target: Address, value?: bigint): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'triggerSelfDestruct',
      args: [target],
      value,
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  async callBurn(account: WalletClient, token: Address, value: bigint): Promise<TransactionReceipt> {
    const txHash = await account.writeContract({
      address: this.address,
      abi: this.abi,
      functionName: 'burn',
      args: [token, value],
      value: 0n,
    })
    return await this.client.waitForTransactionReceipt({ hash: txHash })
  }

  encodeCanReceiveCalldata(): Hex {
    return encodeFunctionData({
      abi: this.abi,
      functionName: 'canReceive',
      args: [],
    })
  }

  encodeDeploymentBytecode(target?: Address, selfDestruct?: boolean): Hex {
    return encodeAbiParameters(
      [
        { name: '_creationBytecode', type: 'bytes' },
        { name: '_target', type: 'address' },
        { name: '_selfdestruct', type: 'bool' },
      ],
      [this.bytecode, target ?? zeroAddress, selfDestruct ?? false],
    )
  }

  encodeCannotReceiveCalldata(): Hex {
    return encodeFunctionData({
      abi: this.abi,
      functionName: 'cannotReceive',
      args: [],
    })
  }

  encodeRelayCalldata(target: Address, amount: bigint, requireSuccess: boolean, data: Hex): Hex {
    return encodeFunctionData({
      abi: this.abi,
      functionName: 'relay',
      args: [target, amount, requireSuccess, data],
    })
  }
}
