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

import { Address, parseAbi } from 'viem'

export class PQ {
  static readonly address: Address = '0x1800000000000000000000000000000000000004'

  static readonly abi = parseAbi([
    'function verifySlhDsaSha2128s(bytes vk, bytes msg, bytes sig) external view returns (bool)',
  ])
}
