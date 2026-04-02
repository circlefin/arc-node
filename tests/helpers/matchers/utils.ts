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

import { bytesToHex } from '@noble/hashes/utils'

/**
 * Customize the output for bigints, and uint8 arrays.
 */
export const jsonHelper = (_key: string, value: unknown) => {
  if (typeof value === 'bigint') {
    return value.toString()
  }
  if (value == null || typeof value !== 'object') {
    return value
  }
  if (value instanceof Uint8Array) {
    return `0x${bytesToHex(value)}`
  }
  return value
}

/**
 * toJsonString is a function that converts a value to a JSON string.
 */
export const toJsonString = (x: unknown, indent?: number) => {
  return JSON.stringify(x, jsonHelper, indent)
}
