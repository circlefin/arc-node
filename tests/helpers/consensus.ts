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

// Client for the consensus-layer (Malachite) RPC server. The localdev testnet
// publishes each validator's RPC on the host at `31000 + validatorIndex`.

const CONSENSUS_RPC_BASE_PORT = 31000

export interface ConsensusValidator {
  address: string
  voting_power: number
  public_key_hex: string
}

export interface ConsensusValidatorSet {
  total_voting_power: number
  count: number
  validators: ConsensusValidator[]
}

export interface ConsensusStatus {
  height: number
  round: number
  validator_set: ConsensusValidatorSet
}

/**
 * RPC URL of a localdev validator's consensus layer. Index `i` maps to
 * `validator${i + 1}`. `LOCALDEV_CL_RPC_URL` overrides the first validator's URL.
 */
export const consensusRpcUrl = (validatorIndex = 0): string => {
  const override = process.env.LOCALDEV_CL_RPC_URL
  if (override && validatorIndex === 0) {
    return override
  }
  return `http://localhost:${CONSENSUS_RPC_BASE_PORT + validatorIndex}`
}

/**
 * Fetch the consensus-layer status, including the current signing validator set.
 */
export const getConsensusStatus = async (url: string = consensusRpcUrl()): Promise<ConsensusStatus> => {
  const response = await fetch(`${url}/status`, {
    headers: { Accept: 'application/vnd.arc.v1+json' },
  })
  if (!response.ok) {
    throw new Error(`Consensus RPC ${url}/status returned ${response.status}`)
  }
  const body: unknown = await response.json()
  return body as ConsensusStatus
}

/**
 * Find a validator in a consensus status by its public key (`0x`-prefixed,
 * case-insensitive). Returns undefined when absent.
 */
export const findConsensusValidator = (status: ConsensusStatus, publicKeyHex: string): ConsensusValidator | undefined =>
  status.validator_set.validators.find((v) => v.public_key_hex.toLowerCase() === publicKeyHex.toLowerCase())

export interface WaitForConsensusStatusOptions {
  url?: string
  timeoutMs?: number
  intervalMs?: number
}

/**
 * Poll the consensus status until `predicate` holds, returning the matching
 * status. Transient fetch failures (e.g. a 503 while the validator set is
 * reconfiguring) are retried; only a timeout throws.
 */
export const waitForConsensusStatus = async (
  predicate: (status: ConsensusStatus) => boolean,
  { url = consensusRpcUrl(), timeoutMs = 60_000, intervalMs = 1_000 }: WaitForConsensusStatusOptions = {},
): Promise<ConsensusStatus> => {
  const deadline = Date.now() + timeoutMs
  let lastError: unknown
  let lastStatus: ConsensusStatus | undefined
  for (;;) {
    try {
      lastStatus = await getConsensusStatus(url)
      lastError = undefined
      if (predicate(lastStatus)) {
        return lastStatus
      }
    } catch (error) {
      lastError = error
    }
    if (Date.now() >= deadline) {
      const detail =
        lastError !== undefined
          ? `last error: ${lastError instanceof Error ? lastError.message : 'unknown error'}`
          : `last validator set count=${lastStatus?.validator_set.count ?? 'unknown'}`
      throw new Error(`Timed out after ${timeoutMs}ms waiting for consensus status at ${url}; ${detail}`)
    }
    await new Promise((resolve) => {
      setTimeout(resolve, intervalMs)
    })
  }
}
