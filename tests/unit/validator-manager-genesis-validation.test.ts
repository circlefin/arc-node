// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

import { expect } from 'chai'
import { schemaValidatorManager } from '../../scripts/genesis/ValidatorManager'

const REGISTRY_ADMIN = '0x1111111111111111111111111111111111111111'
const PVM_ADMIN = '0x2222222222222222222222222222222222222222'
const OWNER = '0x3333333333333333333333333333333333333333'
const PAUSER = '0x4444444444444444444444444444444444444444'
const REGISTERER = '0x5555555555555555555555555555555555555555'
const CONTROLLER_A = '0x6666666666666666666666666666666666666666'
const CONTROLLER_B = '0x7777777777777777777777777777777777777777'

const PUBLIC_KEY_A = `0x${'11'.repeat(32)}`
const PUBLIC_KEY_B = `0x${'22'.repeat(32)}`

const validator = (publicKey: string, controller: string, votingPower: bigint) => ({
  publicKey,
  votingPower,
  controllers: [
    {
      address: controller,
      votingPowerLimit: 100n,
    },
  ],
})

const configWithValidators = (validators: ReturnType<typeof validator>[]) => ({
  proxy: {
    admin: REGISTRY_ADMIN,
  },
  validators,
  PermissionedValidatorManager: {
    proxy: {
      admin: PVM_ADMIN,
    },
    owner: OWNER,
    pauser: PAUSER,
    validatorRegisterers: [REGISTERER],
  },
})

describe('ValidatorManager genesis validator-set validation', () => {
  it('rejects an empty validator set', () => {
    const result = schemaValidatorManager.safeParse(configWithValidators([]))

    expect(result.success).to.be.false
  })

  it('rejects a validator set with no positive voting power', () => {
    const result = schemaValidatorManager.safeParse(
      configWithValidators([validator(PUBLIC_KEY_A, CONTROLLER_A, 0n), validator(PUBLIC_KEY_B, CONTROLLER_B, 0n)]),
    )

    expect(result.success).to.be.false
  })

  it('accepts a validator set with positive voting power', () => {
    const result = schemaValidatorManager.safeParse(configWithValidators([validator(PUBLIC_KEY_A, CONTROLLER_A, 20n)]))

    expect(result.success).to.be.true
  })

  it('accepts zero-power validators when another validator has positive power', () => {
    const result = schemaValidatorManager.safeParse(
      configWithValidators([validator(PUBLIC_KEY_A, CONTROLLER_A, 0n), validator(PUBLIC_KEY_B, CONTROLLER_B, 20n)]),
    )

    expect(result.success).to.be.true
  })
})

describe('ValidatorManager genesis public-key validation', () => {
  // ValidatorRegistry.registerValidator requires exactly 32 bytes; the schema
  // has to say so too, or a bad key only fails later inside the alloc builder.
  const PUBLIC_KEY_31 = `0x${'11'.repeat(31)}`
  const PUBLIC_KEY_33 = `0x${'11'.repeat(33)}`

  it('rejects a 31-byte public key at the schema', () => {
    const result = schemaValidatorManager.safeParse(configWithValidators([validator(PUBLIC_KEY_31, CONTROLLER_A, 20n)]))

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.path.join('.') === 'validators.0.publicKey')).to.be.true
    }
  })

  it('rejects a 33-byte public key at the schema', () => {
    const result = schemaValidatorManager.safeParse(configWithValidators([validator(PUBLIC_KEY_33, CONTROLLER_A, 20n)]))

    expect(result.success).to.be.false
    if (!result.success) {
      expect(result.error.issues.some((issue) => issue.path.join('.') === 'validators.0.publicKey')).to.be.true
    }
  })

  it('accepts a 32-byte public key', () => {
    const result = schemaValidatorManager.safeParse(configWithValidators([validator(PUBLIC_KEY_A, CONTROLLER_A, 20n)]))

    expect(result.success).to.be.true
  })
})
