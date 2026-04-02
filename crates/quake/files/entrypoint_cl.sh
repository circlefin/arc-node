#!/bin/sh

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

# Set up latency emulation
if [ -f /usr/local/bin/latency_setup.sh ]; then
	/usr/local/bin/latency_setup.sh
fi

# If ARC_LOG_FILE is set, stream stdout/stderr to that file using tee
if [ -n "$ARC_LOG_FILE" ]; then
	mkdir -p "$(dirname "$ARC_LOG_FILE")"
	rm -f /tmp/arc-log-pipe
	mkfifo /tmp/arc-log-pipe
	tee -a "$ARC_LOG_FILE" < /tmp/arc-log-pipe &
	exec "/usr/local/bin/arc-node-consensus" "$@" > /tmp/arc-log-pipe 2>&1
else
	exec "/usr/local/bin/arc-node-consensus" "$@"
fi
