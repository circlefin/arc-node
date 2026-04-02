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

import {
  deterministicDeployerProxyAddress,
  fiatTokenProxyAddress,
  multicall3Address,
  permissionedManagerAddress,
  protocolConfigAddress,
  validatorRegistryAddress,
} from '../../genesis'

export const contracts = {
  fiatToken: { address: fiatTokenProxyAddress },
  protocolConfig: { address: protocolConfigAddress },
  validatorRegistry: { address: validatorRegistryAddress },
  permissionedManager: { address: permissionedManagerAddress },
  deterministicDeployerProxy: { address: deterministicDeployerProxyAddress },
  multicall3: { address: multicall3Address },
} as const

export const nativeCurrency = {
  decimals: 18,
  name: 'USDC',
  symbol: 'USDC',
}
