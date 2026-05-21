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

// system contracts
export const fiatTokenProxyAddress = '0x3600000000000000000000000000000000000000' as const
export const protocolConfigAddress = '0x3600000000000000000000000000000000000001' as const
export const validatorRegistryAddress = '0x3600000000000000000000000000000000000002' as const
export const permissionedManagerAddress = '0x3600000000000000000000000000000000000003' as const

// precompiles
export const nativeCoinAutorityAddress = '0x1800000000000000000000000000000000000000' as const
export const nativeCoinControlAddress = '0x1800000000000000000000000000000000000001' as const
export const systemAccountingAddress = '0x1800000000000000000000000000000000000002' as const
export const callFromAddress = '0x1800000000000000000000000000000000000003' as const
export const pqAddress = '0x1800000000000000000000000000000000000004' as const

// predeployed contracts
export const deterministicDeployerProxyAddress = '0x4e59b44847b379578588920ca78fbf26c0b4956c' as const
export const multicall3Address = '0xcA11bde05977b3631167028862bE2a173976CA11' as const
export const multicall3FromAddress = '0x522fAf9A91c41c443c66765030741e4AaCe147D0' as const

// Denylist proxy address. Mainnet uses the next system-contract slot
// (`0x36..04`); other networks use a CREATE2 address mined under the `0x360`
// prefix.
//
// To reproduce a mined address:
//   INIT_CODE_HASH=<hash> make mine-denylist-salt
// Init-code: AdminUpgradeableProxy bytecode + abi.encode(implementation, proxyAdmin, initData).
export const denylistAddressByNetwork = {
  localdev: '0x36059b615370eB999e8eC0c9401835B407834221', // Salt: 0x2e8184e0b708cc70e9f829091612c4c8efef8006ee7527c777e0bd70b64c36c8
  devnet: '0x36061d38f2d939249A947f1254097e0FFC9e2993',
  testnet: '0x360b451bb0490637F52fa1794961455615777757',
  mainnet: '0x3600000000000000000000000000000000000004',
} as const
export const memoAddress = '0x5294E9927c3306DcBaDb03fe70b92e01cCede505' as const
export const gasGuzzlerAddress = '0x45a834A6bB86F516D4157a8cBcc60f2F35F8398C' as const
export const testTokenAddress = '0x298122B4bF05CC897662e535C18417f44C7f274b' as const

// Genesis block coinbase on localdev and devnet; also Quake's default
// `cl_suggested_fee_recipient` fallback (`QUAKE_DEFAULT_FEE_RECIPIENT` in
// `crates/quake/src/setup.rs`).
export const localdevFeeRecipient = '0x65E0a200006D4FF91bD59F9694220dafc49dbBC1' as const
