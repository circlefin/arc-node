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
import { Hex, encodeFunctionData } from 'viem'
import { getClients } from '../helpers'
import { PQ } from '../helpers/PQ'
import { PublicClient } from '@nomicfoundation/hardhat-viem/types'
import pqTestVectors from '../helpers/pq_test_vectors.json'

/**
 * PQ Signature Verifier Precompile Smoke Tests
 *
 * Tests the post-quantum cryptographic signature verification precompile
 * at address 0x1800000000000000000000000000000000000004
 *
 * Supports:
 * - SLH-DSA-SHA2-128s (FIPS 205)
 *
 * To regenerate test vectors (e.g., after updating the crypto libraries):
 *   cargo run --bin generate_pq_test_vectors > tests/helpers/pq_test_vectors.json
 */

describe('PQ Signature Verifier Precompile Smoke Tests', function () {
  let publicClient: PublicClient

  before(async () => {
    const clients = await getClients()
    publicClient = clients.client
  })

  // Helper function to call the precompile via ABI-encoded calldata
  async function callVerify(
    vk: Hex,
    msg: Hex,
    sig: Hex,
  ): Promise<{ result: boolean; gasUsed: bigint; success: boolean; error?: string; details?: string }> {
    try {
      const calldata = encodeFunctionData({
        abi: PQ.abi,
        functionName: 'verifySlhDsaSha2128s',
        args: [vk, msg, sig],
      })

      const result = await publicClient.call({
        to: PQ.address,
        data: calldata,
      })

      // Decode the boolean result
      const isValid = result.data === '0x0000000000000000000000000000000000000000000000000000000000000001'

      // Estimate gas
      const gasEstimate = await publicClient.estimateGas({
        to: PQ.address,
        data: calldata,
      })

      return { result: isValid, gasUsed: gasEstimate, success: true }
    } catch (error: unknown) {
      let errorMessage = ''
      let errorDetails = ''
      if (error && typeof error === 'object') {
        const err = error as Record<string, unknown>
        errorMessage = (err.shortMessage as string) || (err.message as string) || JSON.stringify(err)
        errorDetails = (err.details as string) || ''
      } else {
        errorMessage = String(error)
      }
      return {
        result: false,
        gasUsed: 0n,
        success: false,
        error: errorMessage,
        details: errorDetails,
      }
    }
  }

  describe('SLH-DSA-SHA2-128s Signature Verification', () => {
    it('should verify valid SLH-DSA-SHA2-128s signature', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[0] // First valid test vector
      expect(testVector.is_valid).to.be.true

      const { success, result } = await callVerify(
        testVector.verifying_key as Hex,
        testVector.message as Hex,
        testVector.signature as Hex,
      )

      expect(success).to.be.true
      expect(result).to.be.true // Valid signature should verify
    })

    it('should verify SLH-DSA-SHA2-128s signature with empty message', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[1] // Empty message test vector
      expect(testVector.message).to.equal('0x')
      expect(testVector.is_valid).to.be.true

      const { success, result } = await callVerify(
        testVector.verifying_key as Hex,
        testVector.message as Hex,
        testVector.signature as Hex,
      )

      expect(success).to.be.true
      expect(result).to.be.true // Valid signature on empty message should verify
    })

    it('should reject invalid SLH-DSA-SHA2-128s signature', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[2] // Invalid signature test vector
      expect(testVector.is_valid).to.be.false

      const { success, result } = await callVerify(
        testVector.verifying_key as Hex,
        testVector.message as Hex,
        testVector.signature as Hex,
      )

      expect(success).to.be.true
      expect(result).to.be.false // Invalid signature should not verify
    })

    it('should reject signature with invalid public key length', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[0]
      const invalidPublicKey = ('0x' + '00'.repeat(100)) as Hex

      const { success, details, error } = await callVerify(
        invalidPublicKey,
        testVector.message as Hex,
        testVector.signature as Hex,
      )

      expect(success).to.be.false
      // Stateful precompile reverts with Solidity Error(string); message comes from the node RPC layer.
      expect(`${details ?? ''} ${error ?? ''}`).to.include('Invalid verifying key length')
    })

    it('should reject signature with invalid signature length', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[0]
      const invalidSignature = ('0x' + '00'.repeat(100)) as Hex

      const { success, details, error } = await callVerify(
        testVector.verifying_key as Hex,
        testVector.message as Hex,
        invalidSignature,
      )

      expect(success).to.be.false
      expect(`${details ?? ''} ${error ?? ''}`).to.include('Invalid signature length')
    })
  })

  describe('Gas Usage and Performance', () => {
    it('should have reasonable gas for SLH-DSA-SHA2-128s operations', async () => {
      const testVector = pqTestVectors.slh_dsa_sha2_128s[0]

      const { gasUsed, success } = await callVerify(
        testVector.verifying_key as Hex,
        testVector.message as Hex,
        testVector.signature as Hex,
      )

      expect(success).to.be.true
      expect(gasUsed).to.be.gte(10000n)
    })
  })

  describe('Edge Cases and Error Handling', () => {
    it('should handle calls with no data', async () => {
      try {
        await publicClient.call({
          to: PQ.address,
          data: '0x',
        })
        expect(true).to.be.false
      } catch (error) {
        // Reverting is also acceptable behavior
        expect(error).to.exist
      }
    })

    it('should handle calls with invalid selector', async () => {
      try {
        await publicClient.call({
          to: PQ.address,
          data: '0xdeadbeef',
        })
        expect(true).to.be.false
      } catch (error) {
        // Reverting is expected
        expect(error).to.exist
      }
    })

    it('should handle calls with truncated data', async () => {
      // Encode valid calldata then truncate to just the selector (first 10 hex chars = 4 bytes)
      const calldata = encodeFunctionData({
        abi: PQ.abi,
        functionName: 'verifySlhDsaSha2128s',
        args: ['0x' + '00'.repeat(32), '0x', '0x' + '00'.repeat(100)],
      })
      const selectorOnly = calldata.slice(0, 10) as Hex

      try {
        await publicClient.call({
          to: PQ.address,
          data: selectorOnly,
        })
        expect(true).to.be.false
      } catch (error) {
        // Should revert on malformed input
        expect(error).to.exist
      }
    })
  })
})
