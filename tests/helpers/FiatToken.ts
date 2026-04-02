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

import {
  Account,
  Address,
  Chain,
  Client,
  erc20Abi,
  formatUnits,
  getContract,
  parseAbi,
  parseUnits,
  Transport,
  maxUint256,
} from 'viem'
import { PublicClient, WalletClient } from '@nomicfoundation/hardhat-viem/types'
import { fiatTokenProxyAddress } from '../../scripts/genesis'
import { KeyedClient } from './client-extension'

export const eip2612Abi = [
  {
    inputs: [
      {
        internalType: 'address',
        name: 'owner',
        type: 'address',
      },
    ],
    stateMutability: 'view',
    type: 'function',
    name: 'nonces',
    outputs: [
      {
        internalType: 'uint256',
        name: '',
        type: 'uint256',
      },
    ],
  },
  {
    inputs: [],
    name: 'version',
    outputs: [{ internalType: 'string', name: '', type: 'string' }],
    stateMutability: 'view',
    type: 'function',
  },
] as const

const fiatTokenAbi = [
  ...parseAbi([
    // FiatTokenProxy
    'function admin() view returns(address)',
    'function implementation() view returns(address)',

    // Rescuable
    'function rescuer() view returns (address)',

    'function currency() view returns(string)',
    'function masterMinter() view returns(address)',
    'function minterAllowance(address minter) external view returns (uint256)',
    'function isMinter(address account) external view returns (bool)',
    'function blacklister() view returns (address)',
    'function isBlacklisted(address _account) external view returns (bool)',
    'function pauser() view returns (address)',
    'function paused() view returns (bool)',
    'function owner() external view returns (address)',
    'function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s)',
    'function permit(address owner, address spender, uint256 value, uint256 deadline, bytes memory signature)',

    'function increaseAllowance(address spender, uint256 increment)',
    'function decreaseAllowance(address spender, uint256 decrement)',

    'function DOMAIN_SEPARATOR() external view returns (bytes32)',

    'event Mint(address indexed minter, address indexed to, uint256 amount)',
    'event Burn(address indexed burner, uint256 amount)',
    'event MinterConfigured(address indexed minter, uint256 minterAllowedAmount)',
    'event MinterRemoved(address indexed oldMinter)',
    'event MasterMinterChanged(address indexed newMasterMinter)',
    'event Blacklisted(address indexed account)',
    'event UnBlacklisted(address indexed account)',
    'event BlacklisterChanged(address indexed newBlacklister)',

    // onlyMinters
    'function mint(address _to, uint256 _amount)',
    'function burn(uint256 _amount)',
    // onlyMasterMinter
    'function configureMinter(address minter, uint256 minterAllowedAmount)',
    'function removeMinter(address minter)',
    // onlyBlacklister
    'function blacklist(address _account)',
    'function unBlacklist(address _account)',
    // onlyPauser
    'function pause()',
    'function unpause()',
    // onlyOwner
    'function updateMasterMinter(address _newMasterMinter)',
    'function updateBlacklister(address _newBlacklister)',
    'function transferOwnership(address newOwner)',
    'function updatePauser(address _newPauser)',
    'function updateRescuer(address newRescuer)',
    // onlyRescuer
    'function rescueERC20(address tokenContract, address to, uint256 amount)',
  ]),
  ...erc20Abi,
  ...eip2612Abi,
]

/**
 * USDC is a helper for the FiatToken contract on Arc. It wraps the
 * contract on `fiatTokenProxyAddress`. And handling the
 * convertion from natieve USDC (18 decimals) and ERC20 USDC (6 decimals).
 */
export class USDC {
  static readonly address: Address = fiatTokenProxyAddress
  static readonly abi = fiatTokenAbi
  static readonly decimals = 6

  /**
   * parseUnits parse the ERC20 string amount to subunits.
   * @param amount {string} - the USDC ERC20 string amount, e.g. "0.000001"
   * @returns
   */
  static parseUnits(amount: string): bigint {
    return parseUnits(amount, USDC.decimals)
  }

  /**
   * formatUnits convert the subunits to ERC20 string amount.
   * @param amount {bigint} - the USDC subunits amount.
   * @returns {string} - the USDC ERC20 string amount, e.g. "0.000001"
   */
  static formatUnits(amount: bigint): string {
    return formatUnits(amount, USDC.decimals)
  }

  /**
   * Convert the native amount (18 decimals) to USDC contract amount (6 decimals)
   *
   * @param amount - the native amount (18 decimals)
   * @returns Object with the following properties:
   * - `dustInNative`: the dust native amount (18 decimals)
   * - `notAccurate`: the USDC contract amount (6 decimals), may round up or down.
   * - `roundDown`: the rounded down amount (6 decimals)
   * - `roundUp`: the rounded up amount (6 decimals)
   */
  static fromNative(amount: bigint) {
    if (amount < 0n) throw new Error('not support for negative amount')

    const dustInNative = amount % 1_000_000_000_000n // last 12 decimals
    const roundDown = (amount - dustInNative) / 1_000_000_000_000n
    return {
      nativeDust: dustInNative,
      notAccurate: dustInNative >= 500_000_000_000n ? roundDown + 1n : roundDown,
      roundDown,
      roundUp: dustInNative > 0n ? roundDown + 1n : roundDown,
    }
  }

  /**
   * toNative convert the ERC20 USDC (6 decimals) amount to native USDC (18 decimals) amount.
   *
   * @param amount {bigint} - the USDC ERC20 string amount.
   * @returns {bigint} - the native USDC amount.
   */
  static toNative(amount: bigint): bigint {
    return amount * 1_000_000_000_000n
  }

  /**
   * wrapper for getContract to attch to existing USDC contract.
   *
   * @param client - The client to attach to.
   * @param address - The address of the USDC contract, default to `0x360..00`.
   *
   * @example
   * ```ts
   * const usdc = USDC.attach(client);
   * const balance = await usdc.read.balanceOf([address]);
   * ```
   *
   * ```ts
   * const usdc = USDC.attach(walletClient);
   * const hash = usdc.write.transfer([receiver, amount]);
   * ```
   */
  static attach<
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    const CC extends Client<T, C, A> | KeyedClient<T, C, A>,
  >(client: CC, address: Address = USDC.address) {
    return getContract({ abi: USDC.abi, address, client })
  }
}

export async function signPermit({
  client,
  wallet,
  permitAmount,
  spenderAddress,
  nonce,
  deadline = maxUint256,
  tokenAddress = USDC.address,
}: {
  client: PublicClient
  wallet: WalletClient
  permitAmount: bigint
  deadline?: bigint
  spenderAddress: Address
  nonce?: bigint
  tokenAddress?: Address
}) {
  const token = getContract({
    client,
    address: tokenAddress,
    abi: [...eip2612Abi, ...erc20Abi],
  })

  const signature = await wallet.signTypedData({
    types: {
      Permit: [
        { name: 'owner', type: 'address' },
        { name: 'spender', type: 'address' },
        { name: 'value', type: 'uint256' },
        { name: 'nonce', type: 'uint256' },
        { name: 'deadline', type: 'uint256' },
      ],
    },
    primaryType: 'Permit',
    domain: {
      chainId: client.chain?.id,
      name: await token.read.name(),
      verifyingContract: token.address,
      version: await token.read.version(),
    },
    message: {
      owner: wallet.account.address,
      spender: spenderAddress,
      value: permitAmount,
      nonce: nonce ?? (await token.read.nonces([wallet.account.address])),
      deadline,
    },
  })

  return signature
}
