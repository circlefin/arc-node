#!/usr/bin/env bash

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


# down.sh tears down the local environment

BASE_DIR="$(dirname "$0")"
. "${BASE_DIR}/common.sh"

if [[ -z "${DOCKER_COMPOSE_FLAGS}" ]]; then
  echo 'No DOCKER_COMPOSE_FLAGS set — skipping docker compose down.'
  exit 0
fi

DOWN_FLAGS="--remove-orphans"
if [[ ${CLEAN:-false} == true ]]; then
  DOWN_FLAGS+=" -v"
fi

docker compose ${DOCKER_COMPOSE_FLAGS} down ${DOWN_FLAGS}
