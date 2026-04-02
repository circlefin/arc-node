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


# build-docker.sh builds the Docker image for Arc Node

BASE_DIR="$(dirname "$0")"
. "${BASE_DIR}/common.sh"

# Enable Docker BuildKit for better caching
export DOCKER_BUILDKIT=1
export COMPOSE_DOCKER_CLI_BUILD=1

# Get git version information
export GIT_COMMIT_HASH=$(git rev-parse HEAD 2>/dev/null || echo "0000000000000000000000000000000000000000")
export GIT_VERSION=$(git describe --tags --always --dirty 2>/dev/null || echo "v0.0.0-unknown")
export GIT_SHORT_HASH=$(git rev-parse --short=8 HEAD 2>/dev/null || echo "00000000")

# Build with version information and other build arguments
echo "Building Docker image with build args:"
echo "  BUILD_PROFILE: ${BUILD_PROFILE:-release}"
echo "  GIT_VERSION: ${GIT_VERSION}"
echo "  GIT_COMMIT_HASH: ${GIT_COMMIT_HASH}"
echo "  GIT_SHORT_HASH: ${GIT_SHORT_HASH}"

# Setup authentication, GITHUB_TOKEN in particular
if [ -e .env ] ; then
  set -o allexport && source .env && set +o allexport
fi

# Build the docker images (optional signer compose via DOCKER_COMPOSE_FLAGS in common.sh)
docker compose \
    ${DOCKER_COMPOSE_FLAGS} \
    -f ${PROJECT_DIR}/deployments/arc_execution.yaml \
    -f ${PROJECT_DIR}/deployments/arc_consensus.yaml \
    build \
    --build-arg GIT_COMMIT_HASH="${GIT_COMMIT_HASH}" \
    --build-arg GIT_VERSION="${GIT_VERSION}" \
    --build-arg GIT_SHORT_HASH="${GIT_SHORT_HASH}"
