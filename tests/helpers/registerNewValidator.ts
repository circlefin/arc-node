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
import { PermissionedValidatorManager, ValidatorRegistry, ValidatorStatus } from '../helpers/ValidatorManager'

async function main() {
  const clients = async () => {
    const { client, operator: registerer, getController } = await getClients()
    // first 3 are the validators in localdev genesis
    const validators = await getValidators()
    const validatorRegistry = ValidatorRegistry.attach(client).read
    return { client, registerer, getController, validatorRegistry, validators }
  }

  const { validatorRegistry, registerer, getController, validators } = await clients()

  // Get current validator set
  let validatorSet = await validatorRegistry.getActiveValidatorSet()
  const nextIndex = validatorSet.length

  // Safety check
  if (nextIndex >= validators.length) {
    throw new Error(
      `Not enough validators. Tried to use validators[${nextIndex}] but only ${validators.length} available.`,
    )
  }

  const newValidator = validators[nextIndex]
  const registrationId = await validatorRegistry.getNextRegistrationId()

  if (registrationId !== BigInt(nextIndex + 1)) {
    console.log('Validator already registered, skipping add validator test')
    return
  }

  const controller = getController(registrationId)

  // Before: expect 3 active validators
  expect(validatorSet).to.have.lengthOf(nextIndex)

  // Register new validator
  await PermissionedValidatorManager.attach(registerer)
    .write.registerValidator([newValidator.publicKey])
    .then(ReceiptVerifier.waitSuccess)

  // Activate new validator
  await PermissionedValidatorManager.attach(controller).write.activateValidator().then(ReceiptVerifier.waitSuccess)

  // After: expect 1 more
  validatorSet = await validatorRegistry.getActiveValidatorSet()
  expect(validatorSet).to.have.lengthOf(nextIndex + 1)

  for (const validator of validatorSet) {
    if (validator.publicKey === newValidator.publicKey) {
      expect(validator.votingPower.toString()).to.equal('0')
      expect(validator.status).to.equal(ValidatorStatus.Active)
    }
  }
}
main().catch((err) => {
  console.error(err)
  process.exit(1)
})
