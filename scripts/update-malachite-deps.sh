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

# update-malachite-deps.sh

# Default values
REPO_INPUT="circlefin/malachite"
REF_VALUE=""
REF_TYPE=""
VALIDATE=false

show_usage() {
    echo "Usage: $0 [OPTIONS] <ref-value>"
    echo ""
    echo "Update Malachite dependencies to a specific branch, tag, or commit."
    echo ""
    echo "OPTIONS:"
    echo "  -b, --branch        Specify ref-value as a branch name"
    echo "  -t, --tag           Specify ref-value as a tag"
    echo "  -r, --rev           Specify ref-value as a commit hash"
    echo "  -R, --repo REPO     Repository as OWNER/REPO or full Git URL"
    echo "                      (default: circlefin/malachite)"
    echo "  --validate          Validate that the reference exists remotely"
    echo "  -h, --help          Show this help message"
    echo ""
    echo "REPOSITORY FORMATS:"
    echo "  OWNER/REPO          Short GitHub format (e.g., 'circlefin/malachite')"
    echo "  https://...         Full Git URL (e.g., 'https://github.com/owner/repo.git')"
    echo "  git@github.com:...  SSH Git URL (e.g., 'git@github.com:owner/repo.git')"
    echo ""
    echo "EXAMPLES:"
    echo "  $0 -b main                           # Update to main branch"
    echo "  $0 --branch develop                  # Update to develop branch"
    echo "  $0 -t v1.2.3                         # Update to tag v1.2.3"
    echo "  $0 --tag v2.0.0                      # Update to tag v2.0.0"
    echo "  $0 -r abc123def456                   # Update to specific commit"
    echo "  $0 --rev abc123def456789             # Update to specific commit"
    echo ""
    echo "  # Using OWNER/REPO format:"
    echo "  $0 -b main -R myorg/myrepo"
    echo "  $0 -t v1.0.0 --repo circlefin/malachite"
    echo ""
    echo "  # Using full URLs:"
    echo "  $0 -b main -R https://github.com/myorg/myrepo.git"
    echo "  $0 -t v1.0.0 --repo git@github.com:myorg/myrepo.git"
    echo "  $0 -b main -R https://gitlab.com/myorg/myrepo.git"
    echo ""
    echo "  $0 -t v1.0.0 --validate             # Validate tag exists remotely"
}

is_full_git_url() {
    local repo="$1"
    # Check if it's a full Git URL (https, http, git, ssh protocols)
    if [[ "$repo" =~ ^(https?|git|ssh)://.*\.git$ ]] || [[ "$repo" =~ ^git@.*:.*\.git$ ]]; then
        return 0  # true
    else
        return 1  # false
    fi
}

validate_owner_repo_format() {
    local repo="$1"
    if [[ ! "$repo" =~ ^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+$ ]]; then
        echo "Error: Repository must be in OWNER/REPO format (e.g., 'circlefin/malachite') or a full Git URL"
        return 1
    fi
    return 0
}

normalize_repo_url() {
    local repo_input="$1"

    if is_full_git_url "$repo_input"; then
        # It's already a full URL, return as-is
        echo "$repo_input"
    else
        # It's OWNER/REPO format, validate and convert to GitHub URL
        if validate_owner_repo_format "$repo_input"; then
            echo "https://github.com/${repo_input}.git"
        else
            return 1
        fi
    fi
}

get_display_name() {
    local repo_input="$1"

    if is_full_git_url "$repo_input"; then
        # Extract owner/repo from URL for display
        if [[ "$repo_input" =~ github\.com[:/]([^/]+/[^/]+)(\.git)?$ ]]; then
            echo "${BASH_REMATCH[1]}"
        else
            echo "$repo_input"
        fi
    else
        echo "$repo_input"
    fi
}

validate_ref() {
    local repo_url="$1"
    local ref_type="$2"
    local ref_value="$3"

    echo "Validating $ref_type '$ref_value' exists in repository..."

    case "$ref_type" in
        "branch")
            if git ls-remote --heads "$repo_url" "$ref_value" 2>/dev/null | grep -q "refs/heads/$ref_value$"; then
                echo "✅ Branch '$ref_value' found"
                return 0
            else
                echo "❌ Branch '$ref_value' not found in repository"
                return 1
            fi
            ;;
        "tag")
            if git ls-remote --tags "$repo_url" "$ref_value" 2>/dev/null | grep -q "refs/tags/$ref_value"; then
                echo "✅ Tag '$ref_value' found"
                return 0
            else
                echo "❌ Tag '$ref_value' not found in repository"
                return 1
            fi
            ;;
        "rev")
            # For commits, we can't easily validate without cloning, so just warn
            echo "⚠️  Cannot validate commit hash remotely. Proceeding..."
            return 0
            ;;
    esac
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -b|--branch)
            if [[ -n "$REF_TYPE" ]]; then
                echo "Error: Cannot specify multiple reference types"
                exit 1
            fi
            REF_TYPE="branch"
            REF_VALUE="$2"
            if [[ -z "$REF_VALUE" || "$REF_VALUE" == -* ]]; then
                echo "Error: --branch requires a branch name"
                exit 1
            fi
            shift 2
            ;;
        -t|--tag)
            if [[ -n "$REF_TYPE" ]]; then
                echo "Error: Cannot specify multiple reference types"
                exit 1
            fi
            REF_TYPE="tag"
            REF_VALUE="$2"
            if [[ -z "$REF_VALUE" || "$REF_VALUE" == -* ]]; then
                echo "Error: --tag requires a tag name"
                exit 1
            fi
            shift 2
            ;;
        -r|--rev)
            if [[ -n "$REF_TYPE" ]]; then
                echo "Error: Cannot specify multiple reference types"
                exit 1
            fi
            REF_TYPE="rev"
            REF_VALUE="$2"
            if [[ -z "$REF_VALUE" || "$REF_VALUE" == -* ]]; then
                echo "Error: --rev requires a commit hash"
                exit 1
            fi
            shift 2
            ;;
        -R|--repo)
            REPO_INPUT="$2"
            if [[ -z "$REPO_INPUT" || "$REPO_INPUT" == -* ]]; then
                echo "Error: --repo requires a repository (OWNER/REPO or full URL)"
                exit 1
            fi
            shift 2
            ;;
        --validate)
            VALIDATE=true
            shift
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        -*)
            echo "Error: Unknown option $1"
            show_usage
            exit 1
            ;;
        *)
            # If we haven't set a ref type yet, this might be a positional argument
            if [[ -z "$REF_TYPE" ]]; then
                echo "Error: Must specify reference type with -b, -t, or -r"
                show_usage
                exit 1
            fi
            echo "Error: Unexpected argument: $1"
            show_usage
            exit 1
            ;;
    esac
done

# Validate required arguments
if [[ -z "$REF_TYPE" || -z "$REF_VALUE" ]]; then
    echo "Error: Must specify a reference type and value"
    show_usage
    exit 1
fi

# Normalize repository input to full URL
REPO_URL=$(normalize_repo_url "$REPO_INPUT")
if [[ $? -ne 0 ]]; then
    exit 1
fi

# Get display name for output
REPO_DISPLAY=$(get_display_name "$REPO_INPUT")

# Validate reference exists if requested
if [ "$VALIDATE" = true ]; then
    if ! validate_ref "$REPO_URL" "$REF_TYPE" "$REF_VALUE"; then
        echo "Validation failed. Exiting."
        exit 1
    fi
fi

# List of all malachite dependencies
DEPS=(
    "malachitebft-app"
    "malachitebft-app-channel"
    "malachitebft-codec"
    "malachitebft-config"
    "malachitebft-core-consensus"
    "malachitebft-core-state-machine"
    "malachitebft-core-types"
    # "malachitebft-engine"
    # "malachitebft-metrics"
    "malachitebft-network"
    "malachitebft-peer"
    "malachitebft-proto"
    "malachitebft-signing"
    "malachitebft-signing-ed25519"
    "malachitebft-sync"
    # "malachitebft-test"
    # "malachitebft-test-app"
    # "malachitebft-test-framework"
)

# Check if tomli is available
command -v tomli >/dev/null 2>&1 || {
    echo "Error: tomli is required but not installed."
    echo "Install it via 'cargo install tomli' and try again."
    exit 1
}

# Create a backup
cp Cargo.toml Cargo.toml.bak

echo "Updating Malachite dependencies..."
echo "  Reference:  $REF_TYPE = $REF_VALUE"
echo "  Repository: $REPO_DISPLAY"
echo "  Full URL:   $REPO_URL"
echo ""

# Update each dependency using tomli
for dep in "${DEPS[@]}"; do
    echo "  Updating $dep..."

    # First, remove any existing ref fields (branch, tag, rev)
    tomli delete -i -f Cargo.toml -e "workspace.dependencies.\"$dep\".branch" 2>/dev/null || true
    tomli delete -i -f Cargo.toml -e "workspace.dependencies.\"$dep\".tag"    2>/dev/null || true
    tomli delete -i -f Cargo.toml -e "workspace.dependencies.\"$dep\".rev"    2>/dev/null || true

    # Set the new ref field and git URL
    tomli set -i -f Cargo.toml "workspace.dependencies.\"$dep\".\"$REF_TYPE\"" "$REF_VALUE"
    tomli set -i -f Cargo.toml "workspace.dependencies.\"$dep\".git"           "$REPO_URL"
done

echo ""
echo "✅ Successfully updated all Malachite dependencies!"
echo "   $REF_TYPE: $REF_VALUE"
echo "   Repository: $REPO_DISPLAY"
echo "   Backup saved as Cargo.toml.bak"
echo ""
echo "Next steps:"
echo "  1. Review changes: git diff Cargo.toml"
echo "  2. Update lock file: cargo update"
echo "  3. Test your build: cargo check"
