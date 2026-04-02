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

import hre from 'hardhat'
import { Account, Address, Chain, Client, getContract, Transport } from 'viem'
import { permissionedManagerAddress, validatorRegistryAddress } from '../../scripts/genesis'
import { KeyedClient } from './client-extension'
import { AdminUpgradeableProxy } from './AdminUpgradeableProxy'

export enum ValidatorStatus {
  Unknown = 0,
  Registered = 1,
  Active = 2,
}

export interface ValidatorInfo {
  status: ValidatorStatus
  publicKey: string
  votingPower: bigint
}

export class ValidatorRegistry {
  static address = validatorRegistryAddress

  static attach<
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    const CC extends Client<T, C, A> | KeyedClient<T, C, A>,
  >(client: CC, address: Address = ValidatorRegistry.address) {
    const artifact = hre.artifacts.readArtifactSync('ValidatorRegistry')
    return getContract({ abi: [...artifact.abi, ...AdminUpgradeableProxy.abi], address, client })
  }
}

export class PermissionedValidatorManager {
  static address = permissionedManagerAddress

  static attach<
    T extends Transport,
    C extends Chain | undefined,
    A extends Account | undefined,
    const CC extends Client<T, C, A> | KeyedClient<T, C, A>,
  >(client: CC, address: Address = PermissionedValidatorManager.address) {
    const artifact = hre.artifacts.readArtifactSync('PermissionedValidatorManager')
    return getContract({ abi: [...artifact.abi, ...AdminUpgradeableProxy.abi], address, client })
  }
}
