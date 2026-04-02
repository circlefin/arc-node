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

# Health check script for localstack to ensure all resources are created

export AWS_ACCESS_KEY_ID=foo
export AWS_SECRET_ACCESS_KEY=bar
export AWS_DEFAULT_REGION=us-east-1
export AWS_ENDPOINT_URL=http://localhost:4566

echo "Checking LocalStack health..."

# Check if localstack is running
if ! curl -f http://localhost:4566/_localstack/health > /dev/null 2>&1; then
    echo "LocalStack is not running"
    exit 1
fi

echo "LocalStack is running"

# Check if KMS keys are created
echo "Checking KMS keys..."
if ! aws kms list-aliases --endpoint-url=http://localhost:4566 --region us-east-1 2>/dev/null | grep -q 'alias/dev-multi-region-crypto'; then
    echo "KMS keys are not ready"
    exit 1
fi

echo "KMS keys are ready"

# Check if secret is created
echo "Checking Secrets Manager..."
# '00000000-0000-0000-0000-000000000000' is the hardcoded secret name for testing purposes
if ! aws secretsmanager list-secrets --endpoint-url=http://localhost:4566 --region us-east-1 2>/dev/null | grep -q '00000000-0000-0000-0000-000000000000'; then
    echo "Secrets Manager is not ready"
    exit 1
fi

echo "Secrets Manager is ready"

# Check if replica key exists in us-west-2
echo "Checking replica key in us-west-2..."
if ! aws kms list-aliases --endpoint-url=http://localhost:4566 --region us-west-2 2>/dev/null | grep -q 'alias/dev-multi-region-crypto'; then
    echo "Replica key in us-west-2 is not ready"
    exit 1
fi

echo "All resources are ready!"
exit 0
