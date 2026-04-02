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


# up.sh starts the supporting infrastructure in local-dev

BASE_DIR="$(dirname "$0")"
. "${BASE_DIR}/common.sh"

# No signer compose file (DOCKER_COMPOSE_FLAGS empty) → nothing to start.
if [[ -z "${DOCKER_COMPOSE_FLAGS}" ]]; then
  echo 'No DOCKER_COMPOSE_FLAGS set — skipping docker compose up.'
  exit 0
fi

# Pull latest container images.
if [[ "${PULL_IMAGES:-false}" == "true" ]] ; then
    echo 'Pulling latest container images from ECR.'
    docker compose ${DOCKER_COMPOSE_FLAGS} pull
fi

if [[ "${DOCKER_UP:-true}" = true ]] ; then
  exit_code=0
  time docker compose ${DOCKER_COMPOSE_FLAGS} up --wait || exit_code=$?
  if [[ "$exit_code" -ne 0 ]]; then
    echo "exit on code ${exit_code}, container startup failed"
    . "${BASE_DIR}/export-container-logs.sh"
    exit $exit_code
  fi
fi
