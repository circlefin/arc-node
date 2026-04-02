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


BASE_DIR="$(dirname "$0")"
. "${BASE_DIR}/common.sh"

OUTPUT_DIR="logs"
mkdir -p ${OUTPUT_DIR}

if [ $APP_ENV == 'qa' ]
then
  USER=$(whoami)
  sudo chown $USER ${OUTPUT_DIR}
fi

CONTAINERS=$(docker ps -a --format '{{.Names}}')
for CONTAINER in ${CONTAINERS}; do
    docker logs ${CONTAINER} >& ${OUTPUT_DIR}/${CONTAINER}.log
done

echo "Successfully exported logs in ${OUTPUT_DIR} directory."
