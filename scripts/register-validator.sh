#!/bin/bash

# Copyright 2026 Circle Internet Group, Inc. All rights reserved.
#
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.


set -e

# Usage:
# ./register-validator.sh <validator_public_key>

# Check if the correct number of arguments is provided
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <validator_public_key>"
    exit 1
fi

# Read public key from command line argument
public_key=$1

# CONTROLLER_ADDRESS=0xa0Ee7A142d267C1f36714E4a8F75612F20a79720
# CONTROLLER_KEY=0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6
# VALIDATOR_REGISTERER_KEY=0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6

CONTROLLER_ADDRESS=0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f
CONTROLLER_KEY=0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97
VALIDATOR_REGISTERER_KEY=0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97

PERMISSIONED_VALIDATOR_MANAGER_OWNER=0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97
VALIDATOR_PUBLIC_KEY_BYTES=$public_key

cmd='forge script contracts/scripts/ValidatorManagement.s.sol --rpc-url http://localhost:8645 --broadcast'

echo "Registering validator with public key: $VALIDATOR_PUBLIC_KEY_BYTES"
result=$($cmd --sig "registerValidator()")

# Find registration id from the output
REGISTRATION_ID=$(echo "$result" | ggrep -oP '_registrationId: uint256 \K[0-9]+' | tail -1)

echo "Registered validator with registration id: $REGISTRATION_ID"

sleep 2

echo "Configuring controller at address: $CONTROLLER_ADDRESS"
$cmd --sig "configureController()"

sleep 2

echo "Transferring permissioned validator manager ownership to: $PERMISSIONED_VALIDATOR_MANAGER_OWNER"
$cmd --sig "activateValidator()"

sleep 2

echo "Updating voting power to 20"
$cmd --sig "updateVotingPower(uint64)" 20

sleep 2

echo "Printing active validator set:"
$cmd --sig "printActiveValidatorSet()"
