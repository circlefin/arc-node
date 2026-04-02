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

// predeployed contracts
export const deterministicDeployerProxyAddress = '0x4e59b44847b379578588920ca78fbf26c0b4956c' as const
export const multicall3Address = '0xcA11bde05977b3631167028862bE2a173976CA11' as const
export const multicall3FromAddress = '0xEb7cc06E3D3b5F9F9a5fA2B31B477ff72bB9c8b6' as const

// Denylist proxy address. Deterministic CREATE2-derived with prefix 0x360.
// Init-code: AdminUpgradeableProxy bytecode + abi.encode(implementation, proxyAdmin, initData).
//
// To reproduce:
//   INIT_CODE_HASH=<hash> make mine-denylist-salt
//
// Salt: 0x1ff19f9552a8ba2ba770fc38c8846b30ca47ab7b1caa6cfdfdd3021c1bbe84a4
export const denylistAddress = '0x36082bA812806eB06C2758c412522669b5E2ac7b' as const
export const memoAddress = '0x9702466268ccF55eAB64cdf484d272Ac08d3b75b' as const
export const gasGuzzlerAddress = '0x1be052abb35D7f41609Bfec8F2fC2A684CB9984f' as const
export const testTokenAddress = '0xc35bA063F507CCf914FeEb69c8651ec695872587' as const
