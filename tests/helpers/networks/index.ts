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

import fs from 'fs'
import path from 'path'
import hre from 'hardhat'
import * as localdev from './localdev'
import { schemaGenesisConfig } from '../../../scripts/genesis'

/**
 * Get the clients for the current network
 * @returns The clients for the current network
 */
export const getClients = async () => {
  switch (hre.network.name) {
    case 'localdev':
    case 'local_geth':
      return localdev.getClients()
  }
  throw new Error(`not supported network ${hre.network.name}`)
}

/**
 * Load genesis config from the assets.
 */
export const loadGenesisConfig = () => {
  const configPath = path.join(hre.config.paths.root, 'assets', hre.network.name, 'config.json')
  try {
    return schemaGenesisConfig.parse(JSON.parse(fs.readFileSync(configPath, 'utf-8')))
  } catch (_err) {
    return undefined
  }
}

/**
 * Check if the network is an Arc network
 * @param network The network to check
 * @returns True if the network is an Arc network, false otherwise
 */
export const isArcNetwork = (network?: string) => {
  network = network ?? hre.network.name
  return network !== 'local_geth'
}
