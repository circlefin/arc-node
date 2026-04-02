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
import { ReceiptVerifier, getClients } from '../helpers'
import { getValidators } from '../helpers/networks/localdev'
import {
  PermissionedValidatorManager,
  ValidatorRegistry,
  ValidatorStatus,
  ValidatorInfo,
} from '../helpers/ValidatorManager'
import { PublicClient, WalletClient, parseEther } from 'viem'
import * as ed from '@noble/ed25519'
import { toHex } from 'viem'

describe('ValidatorManager', () => {
  const clients = async () => {
    const { client, admin, operator: registerer, getController, sender, receiver } = await getClients()
    // Load validators for tests that need them
    const validators = await getValidators()
    const validatorRegistry = ValidatorRegistry.attach(client).read
    return { client, admin, registerer, getController, validatorRegistry, validators, sender, receiver }
  }

  const calculateTotalVotingPower = (validatorSet: readonly ValidatorInfo[]) => {
    return validatorSet.reduce((total, validator) => total + validator.votingPower, 0n)
  }

  const calculatePositiveVotingPowerValidatorCount = (validatorSet: readonly ValidatorInfo[]) => {
    return validatorSet.reduce((total, validator) => total + (validator.votingPower > 0n ? 1n : 0n), 0n)
  }

  const finalizeTx = async (
    client: PublicClient,
    sender: WalletClient,
    receiver: WalletClient,
    description: string = 'block progression',
  ): Promise<{ finalBlock: bigint; startBlock: bigint; blocksAdvanced: bigint }> => {
    const startBlock = await client.getBlockNumber()

    // Step 1: Send dummy transfer transaction
    if (!sender.account) {
      expect.fail('Sender account is undefined')
    }
    if (!receiver.account) {
      expect.fail('Receiver account is undefined')
    }
    const txHash = await sender.sendTransaction({
      account: sender.account,
      to: receiver.account.address,
      value: 1n, // 1 wei
      chain: null,
    })

    // Step 2: Wait for transaction to be finalized (included in a block)
    const receipt = await client.waitForTransactionReceipt({ hash: txHash })

    // Step 3: Check block height has advanced
    const finalBlock = receipt.blockNumber
    const blocksAdvanced = finalBlock - startBlock

    if (blocksAdvanced <= 0n) {
      expect.fail(`Block height did not advance in ${description}. Start: ${startBlock}, Final: ${finalBlock}`)
    }

    return { finalBlock, startBlock, blocksAdvanced }
  }

  describe('Genesis Validation', () => {
    it('should validate genesis validator configuration and safety constraints', async () => {
      const { validatorRegistry } = await clients()

      const validatorSet = await validatorRegistry.getActiveValidatorSet()
      expect(validatorSet.length).to.be.greaterThan(0, 'Should have active validators from genesis')

      // Assert genesis voting power matches expected value (20n)
      const expectedGenesisVotingPower = 20n
      for (const validator of validatorSet) {
        expect(validator.votingPower).to.equal(
          expectedGenesisVotingPower,
          `All genesis validators should have voting power of ${expectedGenesisVotingPower}n. Found: ${validator.votingPower}n`,
        )
        expect(validator.status).to.equal(ValidatorStatus.Active, 'All genesis validators should be active')
      }
    })

    it('next registration id', async () => {
      const { validatorRegistry, validators } = await clients()
      const nextRegistrationId = await validatorRegistry.getNextRegistrationId()
      expect(nextRegistrationId).to.be.eq(BigInt(validators.length + 1))
    })
  })

  describe('Basic Validator Management', () => {
    it('should be able to add a new validator', async () => {
      const { client, admin, validatorRegistry, registerer, sender, receiver, getController } = await clients()

      let validatorSet = await validatorRegistry.getActiveValidatorSet()
      const activeValidatorCount = validatorSet.length

      // Generate a new validator
      const privateKey = ed.utils.randomPrivateKey()
      const publicKey = toHex(await ed.getPublicKeyAsync(privateKey))

      // Use the same voting power as existing validators
      const firstValidator = validatorSet[0]
      const votingPower = firstValidator.votingPower
      expect(votingPower).to.be.gt(0n)

      // Step 1: Capture the next registrationId before registering
      const registrationId = await validatorRegistry.getNextRegistrationId()

      // Default voting power assigned by contract
      const expectedInitialVotingPower = 0n

      // Step 2: Register new validator (voting power set by contract default)
      const newValidator = { publicKey }
      await PermissionedValidatorManager.attach(registerer).write.registerValidator([newValidator.publicKey])

      // Create a dynamic controller account based on registrationId
      const newControllerAccount = getController(registrationId, false)

      // Fund the new controller account with ETH for gas fees
      const fundingTx = await sender.sendTransaction({
        account: sender.account,
        to: newControllerAccount.account.address,
        value: parseEther('1'),
        chain: null,
      })
      await client.waitForTransactionReceipt({ hash: fundingTx })

      // Step 3: Configure the controller for the registered validator
      const votingPowerLimit = 10_000n

      await PermissionedValidatorManager.attach(admin) // admin account
        .write.configureController([newControllerAccount.account.address, registrationId, votingPowerLimit])
        .then(ReceiptVerifier.waitSuccess)

      // Verify controller and voting power limit
      const controllerVotingPowerLimit = await PermissionedValidatorManager.attach(admin).read.getVotingPowerLimit([
        newControllerAccount.account.address,
      ])
      expect(controllerVotingPowerLimit).to.equal(votingPowerLimit)

      // Step 4: Verify registered validator voting power is default (0) before activation
      const registeredValidator = await validatorRegistry.getValidator([registrationId])
      expect(registeredValidator.votingPower).to.equal(expectedInitialVotingPower)
      expect(registeredValidator.status).to.equal(ValidatorStatus.Registered)

      // Step 5: Activate the validator using the newly configured controller
      await PermissionedValidatorManager.attach(newControllerAccount)
        .write.activateValidator()
        .then(ReceiptVerifier.waitSuccess)

      // Verify validator was added correctly
      validatorSet = await validatorRegistry.getActiveValidatorSet()
      expect(validatorSet).to.have.lengthOf(activeValidatorCount + 1)

      // Verify the controller has the correct registration ID
      const controllerRegistrationId = await PermissionedValidatorManager.attach(
        newControllerAccount,
      ).read.getRegistrationId([newControllerAccount.account.address])
      expect(controllerRegistrationId.toString()).to.equal(registrationId.toString())

      // Verify validator was added correctly using getValidator
      const activatedValidator = await PermissionedValidatorManager.attach(newControllerAccount).read.getValidator([
        newControllerAccount.account.address,
      ])

      expect(activatedValidator.publicKey).to.equal(newValidator.publicKey)
      expect(activatedValidator.votingPower).to.equal(expectedInitialVotingPower)
      expect(activatedValidator.status).to.equal(ValidatorStatus.Active)

      // Test blockchain health after adding validator
      await finalizeTx(client, sender, receiver, 'blockchain health after adding validator')

      // Clean up: Remove the validator and controller
      await PermissionedValidatorManager.attach(newControllerAccount)
        .write.removeValidator()
        .then(ReceiptVerifier.waitSuccess)

      // Verify validator was removed from active set
      validatorSet = await validatorRegistry.getActiveValidatorSet()
      expect(validatorSet).to.have.lengthOf(activeValidatorCount) // Back to original count
      let foundRemovedValidator = false
      for (const validator of validatorSet) {
        if (validator.publicKey === newValidator.publicKey) {
          foundRemovedValidator = true
          break
        }
      }
      expect(foundRemovedValidator, 'Validator should not be in active set after removal').to.be.false

      // Remove controller configuration
      await PermissionedValidatorManager.attach(admin) // admin account
        .write.removeController([newControllerAccount.account.address])
        .then(ReceiptVerifier.waitSuccess)

      // Verify controller was removed
      const isStillController = await PermissionedValidatorManager.attach(admin).read.isController([
        newControllerAccount.account.address,
      ])
      expect(isStillController, 'Controller should no longer be configured').to.be.false
    })

    it('should update the voting power by controller', async () => {
      const { client, validatorRegistry, getController, validators, sender, receiver } = await clients()
      const validator = validators[2]
      const controller = getController(validator.registrationID)

      // Store original voting power for cleanup
      const originalValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      const originalPower = BigInt(originalValidatorInfo.votingPower)

      // Test incremental voting power increase
      const currentValidator = await validatorRegistry.getValidator([validator.registrationID])
      const oldPower = currentValidator.votingPower
      const newPower = oldPower + 1n

      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([newPower])
        .then(ReceiptVerifier.waitSuccess)

      // Verify the voting power was updated
      const validatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      expect(validatorInfo.votingPower.toString()).to.equal(newPower.toString())

      await finalizeTx(client, sender, receiver, 'blockchain health after voting power increase')

      // Clean up: Reset voting power to original value
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([originalPower])
        .then(ReceiptVerifier.waitSuccess)
    })

    it('should handle voting power set to 0 and back to non-zero', async () => {
      const { client, validatorRegistry, getController, validators, sender, receiver } = await clients()
      const activeValidatorSet = await validatorRegistry.getActiveValidatorSet()
      const expectedPositiveVotingPowerCount = calculatePositiveVotingPowerValidatorCount(activeValidatorSet)
      const trackedPositiveVotingPowerCount = await validatorRegistry.getActiveValidatorsWithPositiveVotingPowerCount()

      expect(trackedPositiveVotingPowerCount).to.equal(
        expectedPositiveVotingPowerCount,
        'Tracked positive voting power count should match active validator set',
      )

      expect(trackedPositiveVotingPowerCount).to.be.gt(
        1n,
        'Setting a validator voting power to 0 requires at least two active validators with positive power',
      )

      const validator = validators[1] // Use second validator to avoid affecting primary validator
      const controller = getController(validator.registrationID)

      // Store original voting power for cleanup
      const originalValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      const originalPower = BigInt(originalValidatorInfo.votingPower)

      expect(originalPower).to.be.gt(0n)
      // Step 1: Set voting power to 0
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([0n])
        .then(ReceiptVerifier.waitSuccess)

      // Verify the voting power was set to 0
      const zeroValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      expect(zeroValidatorInfo.votingPower).to.equal(0n, 'Voting power should be 0')

      // Test blockchain health after setting voting power to 0
      await finalizeTx(client, sender, receiver, 'blockchain health after setting voting power to 0')

      // Step 2: Set voting power back to non-zero (use original + 1 to test the transition)
      const newNonZeroPower = originalPower + 1n
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([newNonZeroPower])
        .then(ReceiptVerifier.waitSuccess)

      // Verify the voting power was restored to non-zero
      const restoredValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      expect(restoredValidatorInfo.votingPower).to.equal(
        newNonZeroPower,
        'Voting power should be restored to non-zero value',
      )

      // Test blockchain health after restoring voting power
      await finalizeTx(client, sender, receiver, 'blockchain health after restoring voting power from 0 to non-zero')

      // Clean up: Reset voting power to original value
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([originalPower])
        .then(ReceiptVerifier.waitSuccess)
    })
  })

  describe('Edge Cases', () => {
    it('Adding validator having >1/3 of total voting power', async () => {
      const { client, validatorRegistry, getController, validators, sender, receiver } = await clients()
      const validator = validators[0]
      const controller = getController(validator.registrationID)

      // Store original voting power for cleanup
      const originalValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      const originalPower = BigInt(originalValidatorInfo.votingPower)

      // Get current validator set

      const validatorSet = await validatorRegistry.getActiveValidatorSet()
      const totalVotingPower = calculateTotalVotingPower(validatorSet as readonly ValidatorInfo[])

      // Calculate more than 1/3 of total voting power
      const moreThanOneThird = (totalVotingPower * 2n) / 5n // 40% of total power

      // Update validator to have >1/3 voting power

      const currentValidatorInfo2 = await validatorRegistry.getValidator([validator.registrationID])

      const currentPower2 = BigInt(currentValidatorInfo2.votingPower)

      // Only update if the power is actually different (avoid contract revert)
      expect(currentPower2).to.not.equal(moreThanOneThird)
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([moreThanOneThird])
        .then(ReceiptVerifier.waitSuccess)

      // Verify the change

      const validatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      expect(validatorInfo.votingPower).to.equal(moreThanOneThird)

      // Test block production - this should still work even with >1/3 voting power
      await finalizeTx(client, sender, receiver, 'blockchain behavior with >1/3 voting power concentration (unsafe)')

      // Clean up: Reset voting power to original value (only if different)
      const finalValidatorInfo = await validatorRegistry.getValidator([validator.registrationID])
      const finalPower = BigInt(finalValidatorInfo.votingPower)

      expect(finalPower).to.not.eq(originalPower)
      await PermissionedValidatorManager.attach(controller)
        .write.updateValidatorVotingPower([originalPower])
        .then(ReceiptVerifier.waitSuccess)
    })
  })
})
