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
import { Hash, Hex, Log } from 'viem'
import { EventsVerifier } from './ReceiptVerifier'
import { BlockCallFrames, CallFrame, CallType } from './client-extension'
import { AddressOrAccount, skipCompare, SkipCompare } from './matchers'

export type ExpectFrameResult<Quantity = bigint> = {
  from: AddressOrAccount | SkipCompare
  to: AddressOrAccount | SkipCompare
  value: Quantity | SkipCompare
  input: Hex | SkipCompare
  output: Hex | SkipCompare | undefined
  gas: Quantity | SkipCompare
  gasUsed?: Quantity | SkipCompare
  type: CallType | SkipCompare
  error: string | RegExp | SkipCompare | undefined
  revertReason: string | RegExp | SkipCompare | undefined
}

export class TraceFrameVerifier {
  constructor(
    public readonly frame: CallFrame,
    public readonly txHash?: Hash,
  ) {}

  static frame = (frame: CallFrame, txHash?: Hash) => {
    const fv = new TraceFrameVerifier(frame, txHash)
    // eslint-disable-next-line @typescript-eslint/no-unsafe-argument,@typescript-eslint/no-explicit-any
    return new Proxy<TraceFrameVerifier & CallFrame>(fv as any, {
      get: (target, prop, receiver) => {
        if (prop in target.frame) {
          return target.frame[prop as keyof CallFrame]
        }
        // eslint-disable-next-line @typescript-eslint/no-unsafe-return
        return Reflect.get(target, prop, receiver)
      },
    })
  }

  static blockFrames = (frames: BlockCallFrames) => {
    return frames.map((frame) => TraceFrameVerifier.frame(frame.result, frame.txHash))
  }

  expectPartialMatch = <Quantity = bigint>(partialFrame: Partial<ExpectFrameResult<Quantity>>) =>
    this.expectMatch({
      from: partialFrame.from ?? skipCompare,
      to: partialFrame.to ?? skipCompare,
      value: partialFrame.value ?? skipCompare,
      input: partialFrame.input ?? skipCompare,
      output: partialFrame.output ?? skipCompare,
      gas: partialFrame.gas ?? skipCompare,
      gasUsed: partialFrame.gasUsed ?? skipCompare,
      type: partialFrame.type ?? skipCompare,
      error: partialFrame.error ?? skipCompare,
      revertReason: partialFrame.revertReason ?? skipCompare,
    })

  expectMatch = <Quantity = bigint>(expectFrame: ExpectFrameResult<Quantity>) => {
    const _d = (field: string) => `${this.txHash ? `tx: ${this.txHash}, ` : ''}frame.${field} mismatched`
    expect(this.frame.from).to.be.addressEqual(expectFrame.from, _d('from'))
    expect(this.frame.to).to.be.addressEqual(expectFrame.to, _d('to'))
    expect(this.frame.value).to.be.eq(expectFrame.value, _d('value'))
    expect(this.frame.input).to.be.eq(expectFrame.input, _d('input'))
    expect(this.frame.output).to.be.eq(expectFrame.output, _d('output'))
    expect(this.frame.gas).to.be.eq(expectFrame.gas, _d('gas'))
    expect(this.frame.gasUsed).to.be.eq(expectFrame.gasUsed, _d('gasUsed'))
    expect(this.frame.type).to.be.eq(expectFrame.type, _d('type'))
    expect(this.frame.error).to.be.eq(expectFrame.error, _d('error'))
    expect(this.frame.revertReason).to.be.eq(expectFrame.revertReason, _d('revertReason'))
    return this
  }

  verifyNoEvents = () => {
    expect(this.frame.logs ?? []).to.have.lengthOf(
      0,
      `${this.txHash ? `tx: ${this.txHash}, ` : ''}frame.logs should be empty`,
    )
    return this
  }

  verifyEvents = (hook: (verifier: EventsVerifier) => void) => {
    const verifier = new EventsVerifier({
      transactionHash: this.txHash,
      logs: (this.frame.logs ?? []).map((log) => {
        const l: Log<bigint, number, true> = {
          address: log.address,
          blockHash: null,
          blockNumber: null,
          topics: log.topics ?? [],
          data: log.data ?? '0x',
          logIndex: null,
          transactionHash: null,
          transactionIndex: null,
          removed: false,
        }
        return l
      }),
    })
    hook(verifier)
    return this
  }

  getSubFrame = (index: number) => {
    const frame = this.frame.calls?.[index]
    if (!frame) {
      throw new Error(`Frame ${index} not found`)
    }
    return TraceFrameVerifier.frame(frame, this.txHash)
  }

  verifySubFrame = (index: number, hook: (verifier: TraceFrameVerifier & CallFrame) => void) => {
    const subframe = this.getSubFrame(index)
    hook(subframe)
    return this
  }
}
