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

/**
 * Contract Version Configuration
 *
 * This file serves as the single source of truth for contract initialization versions.
 * Each version number represents how many times a contract has been initialized/upgraded.
 *
 * Remarks:
 *   - These versions MUST be kept in sync with reinitializer(N) versions in contract code
 *   - Migration scripts needs to validate versions against on-chain storage.
 */

/**
 * ProtocolConfig version
 */
export const PROTOCOL_CONFIG_VERSION = 1n

/**
 * ValidatorRegistry version
 */
export const VALIDATOR_REGISTRY_VERSION = 1n

/**
 * PermissionedValidatorManager version
 */
export const PERMISSIONED_VALIDATOR_MANAGER_VERSION = 1n

/**
 * Denylist version
 */
export const DENYLIST_VERSION = 1n
