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


# common.sh sets good shell defaults and exports helpful environment variables.
# All scripts should start by sourcing this script:
#   BASE_DIR="$(dirname "$0")"
#   . "${BASE_DIR}/common.sh"

# -e exits on error, -u errors on undefined variables, -x sets tracing mode
set -eu

get_abs_filename() {
  # $1 : relative filename
  echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
}

# Basic project paths
export PROJECT_DIR=$(get_abs_filename "$(dirname "$0")/..")
export APP_ENV=${APP_ENV:-"dev"}
export USE_LOCALSTACK=true

# Set AWS profile for dev environment
if [ "${APP_ENV}" == "dev" ];
then
  export AWS_PROFILE_PATH="${HOME}/.aws/credentials"
  if [ -f "${AWS_PROFILE_PATH}" ] && [[ -z "${AWS_PROFILE:-}" ]] ; then
    echo "AWS Profile file exists and AWS_PROFILE env var is not set. Setting AWS_PROFILE to default"
    export AWS_PROFILE="default"
  fi
fi

# Docker Compose configuration: default empty (no signer stack). CI sets DOCKER_COMPOSE_FLAGS
export DOCKER_COMPOSE_FLAGS="${DOCKER_COMPOSE_FLAGS:-}"
