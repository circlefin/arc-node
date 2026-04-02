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

import * as cKzg from 'c-kzg'
import { setupKzg } from 'viem'
import { mainnetTrustedSetupPath } from 'viem/node'

let path = mainnetTrustedSetupPath
// In the CJS build this path resolves to something like:
//
//   node_modules/viem/_cjs/trusted-setups/mainnet.json
//
// But the trusted-setup JSON files are only shipped once, at:
//
//   node_modules/viem/trusted-setups/mainnet.json
//
// This workaround strips the `/_cjs` segment so that the path points to file.
if (path.includes('/_cjs/')) {
  path = path.replace('/_cjs', '')
}

export const kzg = setupKzg(cKzg, path)
