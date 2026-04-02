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
import { createPublicClient, http } from 'viem'
import { hardhat } from 'viem/chains'
import { ValidatorRegistry } from './ValidatorManager.ts'

async function main() {
  const rpcUrl = hre.network.config.url as string

  const client = createPublicClient({
    chain: hardhat,
    transport: http(rpcUrl),
  })

  const contract = ValidatorRegistry.attach(client)
  const result = await contract.read.getActiveValidatorSet()
  console.log('Active Validator Set:', result)
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
