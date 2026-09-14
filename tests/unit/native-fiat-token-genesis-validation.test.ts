// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0

import { expect } from 'chai'
import { schemaNativeFiatToken } from '../../scripts/genesis/NativeFiatToken'

const PROXY_ADMIN = '0x1111111111111111111111111111111111111111'
const OWNER = '0x2222222222222222222222222222222222222222'
const PAUSER = '0x3333333333333333333333333333333333333333'
const BLACKLISTER = '0x4444444444444444444444444444444444444444'
const MASTER_MINTER = '0x5555555555555555555555555555555555555555'
const RESCUER = '0x6666666666666666666666666666666666666666'

const configWithMinters = (minters: Array<{ address: string; allowance: bigint }>) => ({
  proxy: { admin: PROXY_ADMIN },
  owner: OWNER,
  pauser: PAUSER,
  blacklister: BLACKLISTER,
  masterMinter: MASTER_MINTER,
  rescuer: RESCUER,
  minters,
})

describe('NativeFiatToken genesis validation', () => {
  it('rejects duplicate minters with identical casing', () => {
    const minter = '0xAb00000000000000000000000000000000000001'
    const result = schemaNativeFiatToken.safeParse(
      configWithMinters([
        { address: minter, allowance: 100n },
        { address: minter, allowance: 200n },
      ]),
    )

    expect(result.success).to.be.false
  })

  it('rejects duplicate minters when address casing differs', () => {
    const minter = '0xAb00000000000000000000000000000000000001'
    const result = schemaNativeFiatToken.safeParse(
      configWithMinters([
        { address: minter, allowance: 100n },
        { address: minter.toLowerCase(), allowance: 200n },
      ]),
    )

    expect(result.success).to.be.false
  })

  it('accepts distinct minter addresses', () => {
    const result = schemaNativeFiatToken.safeParse(
      configWithMinters([
        { address: '0xAb00000000000000000000000000000000000001', allowance: 100n },
        { address: '0xAb00000000000000000000000000000000000002', allowance: 200n },
      ]),
    )

    expect(result.success).to.be.true
  })

  it('rejects a role colliding with the proxy admin when address casing differs', () => {
    const proxyAdmin = '0xAb00000000000000000000000000000000000003'
    const result = schemaNativeFiatToken.safeParse({
      ...configWithMinters([]),
      proxy: { admin: proxyAdmin },
      owner: proxyAdmin.toLowerCase(),
    })

    expect(result.success).to.be.false
  })
})
