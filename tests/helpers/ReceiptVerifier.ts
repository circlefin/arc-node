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
import { assert, expect } from 'chai'
import {
  Abi,
  Address,
  ContractEventName,
  EncodeEventTopicsParameters,
  erc20Abi,
  Hash,
  Hex,
  Log,
  parseEventLogs,
  TransactionReceipt,
} from 'viem'
import { AddressOrAccount, expectAddressEq, parseAddress, toJsonString } from './matchers'
import { Denylist } from './Denylist'
import { NativeCoinAuthority } from './NativeCoinAuthority'
import { NativeCoinControl } from './NativeCoinControl'
import { USDC } from './FiatToken'
import { CallHelper, CallResult } from './CallHelper'
import { getClients, isArcNetwork } from './networks'
import { memoAddress } from '../../scripts/genesis'
import { schemaBytes32 } from '../../scripts/genesis/types'

const memoAbi = hre.artifacts.readArtifactSync('Memo').abi

export const allTestAbis = [
  ...CallHelper.abi,
  ...Denylist.abi,
  ...USDC.abi,
  ...NativeCoinAuthority.abi,
  ...NativeCoinControl.abi,
  ...memoAbi,
]

/** EIP-7708 system address for native transfer log emission. */
const EIP7708_SYSTEM_ADDRESS: Address = '0xFfffFfFFFfFFFFfFfFffFFFfFfFfFFfFFfFFFFFe'

const GAS_DISCREPANCY_DELTA_RATIO = 0.05

export class ReceiptVerifier {
  constructor(public readonly receipt: TransactionReceipt) {}

  static wait = async (hash: Hash) => {
    const { client } = await getClients()
    const receipt = await client.waitForTransactionReceipt({ hash: schemaBytes32.parse(hash) })
    return ReceiptVerifier.build(receipt)
  }

  static waitSuccess = async (hash: Hash) =>
    ReceiptVerifier.wait(hash).then((rv) => {
      rv.isSuccess()
      return rv
    })

  static build = (receipt: TransactionReceipt) => {
    const rv = new ReceiptVerifier(receipt)
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument,@typescript-eslint/no-explicit-any
    return new Proxy<ReceiptVerifier & TransactionReceipt>(rv as any, {
      get: (target, prop, receiver) => {
        if (prop in target.receipt) {
          return target.receipt[prop as keyof TransactionReceipt]
        }
        // eslint-disable-next-line @typescript-eslint/no-unsafe-return
        return Reflect.get(target, prop, receiver)
      },
    })
  }

  totalFee = () => this.receipt.gasUsed * this.receipt.effectiveGasPrice

  isReverted = () => {
    expect(this.receipt.status, `tx ${this.receipt.transactionHash} not reverted`).to.eq('reverted')
    return this
  }

  isSuccess = () => {
    expect(this.receipt.status, `tx ${this.receipt.transactionHash} not success`).to.eq('success')
    return this
  }

  verifyNoEvents = () => {
    const verifier = new EventsVerifier(this.receipt)
    expect(this.receipt.logs, toJsonString(verifier.parsedLogs())).to.have.lengthOf(0)
    return this
  }

  verifyEvents = (hook: (verifier: EventsVerifier) => void) => {
    const verifier = new EventsVerifier(this.receipt)
    hook(verifier)
    return this
  }

  verifyGasUsed = (gasUsed: bigint) => {
    expect(this.receipt.gasUsed, `tx ${this.receipt.transactionHash} gasUsed mismatched`).to.be.eq(gasUsed)
    return this
  }

  verifyGasUsedApproximately = (gasUsed: bigint, deltaRatio: number = GAS_DISCREPANCY_DELTA_RATIO) => {
    expectGasUSedApproximately(
      this.receipt.gasUsed,
      gasUsed,
      deltaRatio,
      `tx ${this.receipt.transactionHash} gasUsed mismatched approximately`,
    )
    return this
  }
}

export const expectGasUSedApproximately = (
  actualGasUsed: bigint,
  expectedGasUsed: bigint,
  deltaRatio: number = GAS_DISCREPANCY_DELTA_RATIO,
  msg?: string,
) => {
  const actual = Number(actualGasUsed)
  expect(actual, msg).to.be.approximately(Number(expectedGasUsed), actual * deltaRatio)
}

export class EventsVerifier {
  constructor(
    public readonly receipt: TransactionReceipt | { transactionHash?: Hash; logs: Log<bigint, number, true>[] },
    public readonly prefixMessage?: string,
  ) {}

  static fromSimulationLogs(logs?: Log[]) {
    return new EventsVerifier({
      logs: (logs ?? []).map((x) => ({
        blockHash: null,
        blockNumber: null,
        logIndex: null,
        transactionHash: null,
        transactionIndex: null,
        address: x.address ?? '0x',
        data: x.data ?? '0x',
        removed: false,
        topics: x.topics ?? [],
      })),
    })
  }

  /**
   * parsedLogs parses the logs of the receipt.
   */
  parsedLogs = <TAbi extends Abi | readonly unknown[]>(abi?: TAbi) => {
    const _abi = [...(abi ?? []), ...allTestAbis]
    return this.receipt.logs.map((log) => {
      const parsed = parseEventLogs({ abi: _abi, logs: [log] })
      if (parsed.length === 0) {
        return log
      }
      return parsed[0]
    })
  }

  _d = (desc: string) =>
    `${this.prefixMessage ?? ''}${this.receipt.transactionHash ? `tx ${this.receipt.transactionHash}, ` : ''}${desc}`

  expectCount = (count: number) => {
    expect(this.receipt.logs, toJsonString(this.parsedLogs())).to.have.lengthOf(count, this._d('logs count mismatched'))
    return this
  }

  private _currentLogIndex = 0

  static FIND_BY_NAME = -1

  setLogIndex = (index: number) => {
    this._currentLogIndex = index
    return this
  }

  expectAllEventsMatched = () => {
    expect(this.receipt.logs, toJsonString(this.parsedLogs())).to.have.lengthOf(
      this._currentLogIndex,
      this._d('logs count mismatched'),
    )
  }

  /**
   * verifyEvent verify the event in the receipt logs.
   *
   * @param e {Object} - expect event, must contain `abi`, `address`, `eventName`, `args`
   * @param index {number?} - index of the event in the receipt logs, if not provided, will search all logs for the eventName.
   */
  expectEvent = <
    const TAbi extends Abi | readonly unknown[],
    EventName extends ContractEventName<TAbi> | undefined = undefined,
  >(
    contractAddress: Address,
    e: EncodeEventTopicsParameters<TAbi, EventName>,
    index?: number,
  ) => {
    const _d = (desc: string) => this._d(desc)
    index = index ?? this._currentLogIndex++
    const event = (() => {
      if (index == EventsVerifier.FIND_BY_NAME) {
        const ev = parseEventLogs<TAbi, true, EventName>({
          abi: e.abi,
          logs: this.receipt.logs,
        }).find((x) => 'eventName' in x && x.eventName === (e.eventName as string))
        expect(ev, _d(`event ${e.eventName} not found`)).to.exist
        index = undefined
        return ev
      }
      const evLog = this.receipt.logs[index]
      expect(evLog, _d(`event ${e.eventName} at index ${index} not found`)).to.exist
      const ev = parseEventLogs({ abi: e.abi, logs: [evLog] })[0]
      expect(ev, _d(`event ${e.eventName} at index ${index} ABI or event name mismatched`)).to.exist
      return ev
    })()

    assert(event != null)
    expectAddressEq(event.address, contractAddress, _d(`event address at index ${index} mismatch`))
    if ('eventName' in event) {
      expect(event.eventName, _d(`name mismatch at index ${index}`)).to.be.eq(e.eventName)
      expect(event.args, _d(`${e.eventName} at index ${index} args mismatch`)).to.deep.eq(e.args)
    } else {
      expect(event, _d(`event parse failed`)).to.be.undefined
    }
    return this
  }

  expectUSDCTransfer = (data: { from: AddressOrAccount; to: AddressOrAccount; value: bigint }, index?: number) =>
    this.expectERC20Transfer(USDC.address, data, index)

  expectERC20Transfer = (
    address: Address,
    data: { from: AddressOrAccount; to: AddressOrAccount; value: bigint },
    index?: number,
  ) =>
    this.expectEvent(
      address,
      {
        abi: erc20Abi,
        args: { ...data, from: parseAddress(data.from), to: parseAddress(data.to) },
        eventName: 'Transfer',
      },
      index,
    )

  expectUSDCMint = (data: { minter: AddressOrAccount; to: AddressOrAccount; amount: bigint }, index?: number) =>
    this.expectEvent(
      USDC.address,
      {
        abi: USDC.abi,
        args: { ...data, minter: parseAddress(data.minter), to: parseAddress(data.to) },
        eventName: 'Mint',
      },
      index,
    )

  expectUSDCBurn = (data: { burner: AddressOrAccount; amount: bigint }, index?: number) =>
    this.expectEvent(
      USDC.address,
      { abi: USDC.abi, args: { ...data, burner: parseAddress(data.burner) }, eventName: 'Burn' },
      index,
    )

  expectNativeMint = (data: { recipient: AddressOrAccount; amount: bigint }, index?: number) =>
    this.expectERC20Transfer(
      EIP7708_SYSTEM_ADDRESS,
      { from: '0x0000000000000000000000000000000000000000', to: parseAddress(data.recipient), value: data.amount },
      index,
    )

  expectNativeBurn = (data: { from: AddressOrAccount; amount: bigint }, index?: number) =>
    this.expectERC20Transfer(
      EIP7708_SYSTEM_ADDRESS,
      { from: parseAddress(data.from), to: '0x0000000000000000000000000000000000000000', value: data.amount },
      index,
    )

  expectUSDCApproval = (data: { owner: AddressOrAccount; spender: AddressOrAccount; value: bigint }, index?: number) =>
    this.expectEvent(
      USDC.address,
      {
        abi: USDC.abi,
        args: { ...data, owner: parseAddress(data.owner), spender: parseAddress(data.spender) },
        eventName: 'Approval',
      },
      index,
    )

  expectNativeTransfer = (data: { from: AddressOrAccount; to: AddressOrAccount; amount: bigint }, index?: number) => {
    if (!isArcNetwork(hre.network.name)) {
      return this
    }
    return this.expectERC20Transfer(
      EIP7708_SYSTEM_ADDRESS,
      { from: parseAddress(data.from), to: parseAddress(data.to), value: data.amount },
      index,
    )
  }

  expectBeforeMemo = (data: { memoIndex: bigint }, index?: number) =>
    this.expectEvent(
      memoAddress,
      {
        abi: memoAbi,
        eventName: 'BeforeMemo',
        args: data,
      },
      index,
    )

  expectMemo = (
    data: {
      sender: AddressOrAccount
      target: AddressOrAccount
      callDataHash: Hash
      memoId: Hash
      memo: Hex
      memoIndex: bigint
    },
    index?: number,
  ) => {
    const args = { ...data, sender: parseAddress(data.sender), target: parseAddress(data.target) }
    return this.expectEvent(memoAddress, { abi: memoAbi, eventName: 'Memo', args }, index)
  }

  expectDenylisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      Denylist.address,
      {
        abi: Denylist.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'Denylisted',
      },
      index,
    )

  expectUnDenylisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      Denylist.address,
      {
        abi: Denylist.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'UnDenylisted',
      },
      index,
    )

  expectNativeBlocklisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      NativeCoinControl.address,
      {
        abi: NativeCoinControl.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'Blocklisted',
      },
      index,
    )

  expectUSDCBlacklisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      USDC.address,
      {
        abi: USDC.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'Blacklisted',
      },
      index,
    )

  expectNativeUnBlocklisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      NativeCoinControl.address,
      {
        abi: NativeCoinControl.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'UnBlocklisted',
      },
      index,
    )

  expectUSDCUnBlacklisted = (data: { account: AddressOrAccount }, index?: number) =>
    this.expectEvent(
      USDC.address,
      {
        abi: USDC.abi,
        args: { ...data, account: parseAddress(data.account) },
        eventName: 'UnBlacklisted',
      },
      index,
    )

  expectTransferEventCount = (count: number) => {
    const events = parseEventLogs({
      abi: erc20Abi,
      logs: this.receipt.logs,
    }).filter((x) => x.eventName === 'Transfer' && x.address.toLowerCase() === EIP7708_SYSTEM_ADDRESS.toLowerCase())
    expect(events.length, toJsonString(this.parsedLogs())).to.eq(count)
    return this
  }

  expectExecutionContext = (
    { helper, sender, ...rest }: { helper: AddressOrAccount; sender: AddressOrAccount; value: bigint },
    index?: number,
  ) =>
    this.expectEvent(
      parseAddress(helper),
      {
        abi: CallHelper.abi,
        args: { ...rest, sender: parseAddress(sender) },
        eventName: 'ExecutionContext',
      },
      index,
    )

  expectExecutionResult = ({ helper, ...result }: { helper: AddressOrAccount } & CallResult, index?: number) =>
    this.expectEvent(
      parseAddress(helper),
      {
        abi: CallHelper.abi,
        args: { ...CallHelper.result(result) },
        eventName: 'ExecutionResult',
      },
      index,
    )

  getExecutionResult = (index: number) => {
    if (index < 0 || index >= this.receipt.logs.length) {
      throw new Error(`index out of range: ${index}`)
    }
    const parsed = parseEventLogs({
      abi: CallHelper.abi,
      logs: this.receipt.logs.slice(index, index + 1),
    })[0]
    expect(parsed.eventName).to.be.eq('ExecutionResult')
    if (parsed.eventName === 'ExecutionResult') {
      return parsed.args.result
    } else {
      return '0x'
    }
  }

  expectCallHelperStorageSet = (
    {
      helper,
      sender,
      slot,
      value,
    }: { helper: AddressOrAccount; sender: AddressOrAccount; slot: bigint; value: bigint },
    index?: number,
  ) =>
    this.expectEvent(
      parseAddress(helper),
      { abi: CallHelper.abi, args: { sender: parseAddress(sender), slot, value }, eventName: 'StorageSet' },
      index,
    )
}
