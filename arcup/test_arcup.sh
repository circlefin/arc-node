#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

set --
ARCUP_SKIP_MAIN=1
source "$ROOT_DIR/arcup/arcup"
GITHUB_AUTH_TOKEN=""
CURL_HEADERS=()

TEST_TMP="$(mktemp -d)"
cleanup() {
    rm -rf "$TEST_TMP"
}
trap cleanup EXIT

pass() {
    printf 'ok - %s\n' "$1"
}

fail() {
    printf 'not ok - %s\n' "$1" >&2
    exit 1
}

assert_eq() {
    local expected="$1"
    local actual="$2"
    local name="$3"

    if [[ "$expected" != "$actual" ]]; then
        printf 'expected: %s\nactual:   %s\n' "$expected" "$actual" >&2
        fail "$name"
    fi
    pass "$name"
}

expect_fail() {
    local name="$1"
    shift

    if ( "$@" ) >"$TEST_TMP/expect_fail.out" 2>&1; then
        cat "$TEST_TMP/expect_fail.out" >&2
        fail "$name"
    fi
    pass "$name"
}

test_version_normalization() {
    assert_eq "v1.2.3" "$(normalize_version "1.2.3")" "normalizes missing v prefix"
    assert_eq "v1.2.3" "$(normalize_version "v1.2.3")" "keeps v-prefixed version"
    assert_eq "v1.2.3-rc.1" "$(normalize_version "1.2.3-rc.1")" "normalizes prerelease version"
    expect_fail "rejects invalid version" normalize_version "latest"
}

test_version_comparison() {
    if ! version_gt "0.3.1-rc.1" "0.3.0"; then
        fail "compares prerelease installer versions"
    fi
    pass "compares prerelease installer versions"

    if version_gt "0.3.0-rc.1" "0.3.0"; then
        fail "same prerelease base is not newer"
    fi
    pass "same prerelease base is not newer"
}

test_target_mapping() {
    assert_eq "x86_64-unknown-linux-gnu" "$(detect_target "linux" "amd64")" "maps linux amd64 target"
    assert_eq "aarch64-unknown-linux-gnu" "$(detect_target "linux" "arm64")" "maps linux arm64 target"
    assert_eq "aarch64-apple-darwin" "$(detect_target "darwin" "arm64")" "maps macos arm64 target"
    expect_fail "rejects macos amd64 target" detect_target "darwin" "amd64"
}

test_unknown_architecture_fails() {
    if (
        uname() {
            case "${1:-}" in
                -m) echo "sparc64" ;;
                -s) echo "Linux" ;;
                *) command uname "$@" ;;
            esac
        }
        detect_arch
    ) >"$TEST_TMP/unknown_arch.out" 2>&1; then
        cat "$TEST_TMP/unknown_arch.out" >&2
        fail "unsupported architecture fails"
    fi
    pass "unsupported architecture fails"
}

test_github_api_url_rejects_non_https() {
    local out="$TEST_TMP/github-api-url.out"

    if GITHUB_API_URL="http://api.github.com" ARCUP_SKIP_MAIN=1 bash "$ROOT_DIR/arcup/arcup" >"$out" 2>&1; then
        cat "$out" >&2
        fail "rejects non-https GitHub API URL"
    fi

    if ! grep -q "invalid GITHUB_API_URL" "$out"; then
        cat "$out" >&2
        fail "rejects non-https GitHub API URL"
    fi
    pass "rejects non-https GitHub API URL"
}

test_checksum_validation() {
    local archive="$TEST_TMP/archive.tar.gz"
    local checksum_file="$TEST_TMP/archive.tar.gz.sha256"
    local archive_name="arc-node-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"
    local checksum

    printf 'archive bytes' > "$archive"
    checksum="$(compute_sha256 "$archive")"
    printf '%s  %s\n' "$checksum" "$archive_name" > "$checksum_file"
    verify_checksum_file "$archive" "$checksum_file" "$archive_name"
    pass "valid checksum file passes"

    printf '%s  other-asset.tar.gz\n' "$checksum" > "$checksum_file"
    expect_fail "checksum filename mismatch fails" verify_checksum_file "$archive" "$checksum_file" "$archive_name"
}

test_download_error_lists_assets() {
    local out="$TEST_TMP/download_error.out"

    if (
        GITHUB_AUTH_TOKEN=""
        CURL_HEADERS=()
        TARGET="x86_64-unknown-linux-gnu"
        gh() { return 1; }
        curl_with_headers() { return 22; }
        list_release_assets() {
            printf '%s\n' \
                "arc-node-v1.2.3-x86_64-unknown-linux-gnu.tar.gz" \
                "arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"
        }
        download_file "v1.2.3" "arc-node-v1.2.3-missing.tar.gz" "$TEST_TMP"
    ) >"$out" 2>&1; then
        cat "$out" >&2
        fail "download error fails"
    fi

    if ! grep -q "Available arc-node release assets" "$out"; then
        cat "$out" >&2
        fail "download error lists available assets"
    fi
    pass "download error lists available assets"
}

test_download_file_uses_github_token_api() {
    local fakebin="$TEST_TMP/token-api-fakebin"
    local release_dir="$TEST_TMP/token-api-release"
    local output_dir="$TEST_TMP/token-api-output"
    local asset_name="arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"

    mkdir -p "$fakebin" "$release_dir" "$output_dir"
    printf 'token asset\n' > "$release_dir/$asset_name"

    cat > "$fakebin/gh" <<'EOF'
#!/usr/bin/env bash
echo "gh should not be called when token API succeeds" >&2
exit 99
EOF
    chmod 755 "$fakebin/gh"

    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

out=""
dump=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o)
            out="$2"
            shift 2
            ;;
        -D)
            dump="$2"
            shift 2
            ;;
        -H | --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

case "$url" in
    *api.github.com/repos/test/arc-node/releases/tags/v1.2.3)
        data='{"assets":[{"url":"https://api.github.com/repos/test/arc-node/releases/assets/123","name":"arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"}]}'
        ;;
    *api.github.com/repos/test/arc-node/releases/assets/123)
        if [[ "$dump" == "-" ]]; then
            printf 'HTTP/1.1 302 Found\r\nLocation: https://objects.example/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz\r\n\r\n'
            exit 0
        fi
        printf 'asset API request should only resolve redirect location\n' >&2
        exit 22
        ;;
    *objects.example/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz)
        cp "$TOKEN_RELEASE_DIR/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz" "$out"
        exit 0
        ;;
    *releases/download*)
        echo "direct release URL should not be called when token API succeeds" >&2
        exit 99
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac

if [[ -n "$out" ]]; then
    printf '%s\n' "$data" > "$out"
else
    printf '%s\n' "$data"
fi
EOF
    chmod 755 "$fakebin/curl"

    (
        PATH="$fakebin:$PATH"
        export TOKEN_RELEASE_DIR="$release_dir"
        GITHUB_AUTH_TOKEN="test-token"
        CURL_HEADERS=()
        REPO="test/arc-node"
        download_file "v1.2.3" "$asset_name" "$output_dir"
    ) >"$TEST_TMP/token-api.out" 2>&1

    if [[ "$(cat "$output_dir/$asset_name")" != "token asset" ]]; then
        cat "$TEST_TMP/token-api.out" >&2
        fail "download_file uses token API when available"
    fi
    pass "download_file uses token API when available"
}

test_token_api_preserves_custom_header() {
    local fakebin="$TEST_TMP/token-header-fakebin"
    local release_dir="$TEST_TMP/token-header-release"
    local output_dir="$TEST_TMP/token-header-output"
    local asset_name="arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"

    mkdir -p "$fakebin" "$release_dir" "$output_dir"
    printf 'token header asset\n' > "$release_dir/$asset_name"

    cat > "$fakebin/gh" <<'EOF'
#!/usr/bin/env bash
echo "gh should not be called when token API succeeds" >&2
exit 99
EOF
    chmod 755 "$fakebin/gh"

    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

out=""
dump=""
url=""
headers=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o)
            out="$2"
            shift 2
            ;;
        -D)
            dump="$2"
            shift 2
            ;;
        -H)
            headers="${headers}${2}
"
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

require_header() {
    local expected="$1"
    if [[ "$headers" != *"$expected"* ]]; then
        printf 'missing header %s for %s\nheaders:\n%s\n' "$expected" "$url" "$headers" >&2
        exit 22
    fi
}

case "$url" in
    *api.github.com/repos/test/arc-node/releases/tags/v1.2.3)
        require_header "Authorization: Bearer test-token"
        require_header "X-Arc-Test: custom"
        require_header "Accept: application/vnd.github+json"
        data='{"assets":[{"url":"https://api.github.com/repos/test/arc-node/releases/assets/123","name":"arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"}]}'
        ;;
    *api.github.com/repos/test/arc-node/releases/assets/123)
        require_header "Authorization: Bearer test-token"
        require_header "X-Arc-Test: custom"
        require_header "Accept: application/octet-stream"
        if [[ "$dump" == "-" ]]; then
            printf 'HTTP/2 302\r\nlocation: https://objects.example/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz\r\n\r\n'
            exit 0
        fi
        printf 'asset API request should only resolve redirect location\n' >&2
        exit 22
        ;;
    *objects.example/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz)
        if [[ "$headers" == *"Authorization:"* || "$headers" == *"X-Arc-Test:"* ]]; then
            printf 'signed asset URL received leaked headers:\n%s\n' "$headers" >&2
            exit 22
        fi
        cp "$TOKEN_HEADER_RELEASE_DIR/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz" "$out"
        exit 0
        ;;
    *releases/download*)
        echo "direct release URL should not be called when token API succeeds" >&2
        exit 99
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac

if [[ -n "$out" ]]; then
    printf '%s\n' "$data" > "$out"
else
    printf '%s\n' "$data"
fi
EOF
    chmod 755 "$fakebin/curl"

    (
        PATH="$fakebin:$PATH"
        export TOKEN_HEADER_RELEASE_DIR="$release_dir"
        GITHUB_AUTH_TOKEN="test-token"
        CURL_HEADERS=(-H "X-Arc-Test: custom")
        REPO="test/arc-node"
        download_file "v1.2.3" "$asset_name" "$output_dir"
    ) >"$TEST_TMP/token-header.out" 2>&1

    if [[ "$(cat "$output_dir/$asset_name")" != "token header asset" ]]; then
        cat "$TEST_TMP/token-header.out" >&2
        fail "token API preserves custom header"
    fi
    pass "token API preserves custom header"
}

test_latest_version_retries_anonymous_after_token_failure() {
    local fakebin="$TEST_TMP/latest-fallback-fakebin"
    local out="$TEST_TMP/latest-fallback.out"

    mkdir -p "$fakebin"
    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

headers=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -H)
            headers="${headers}${2}
"
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

case "$url" in
    *api.github.com/repos/test/arc-node/releases/latest)
        if [[ "$headers" == *"Authorization:"* ]]; then
            printf 'bad token\n' >&2
            exit 22
        fi
        printf '{"tag_name":"v1.2.3"}\n'
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac
EOF
    chmod 755 "$fakebin/curl"

    (
        PATH="$fakebin:$PATH"
        GITHUB_AUTH_TOKEN="stale-token"
        CURL_HEADERS=()
        REPO="test/arc-node"
        GITHUB_API_URL="https://api.github.com"
        version="$(get_latest_version)"
        [[ "$version" == "v1.2.3" ]]
    ) >"$out" 2>&1 || {
        cat "$out" >&2
        fail "latest version retries anonymously after token failure"
    }

    pass "latest version retries anonymously after token failure"
}

test_latest_version_redacts_authenticated_failure() {
    local fakebin="$TEST_TMP/latest-redaction-fakebin"
    local out="$TEST_TMP/latest-redaction.out"

    mkdir -p "$fakebin"
    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

headers=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -H)
            headers="${headers}${2}
"
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

case "$url" in
    *api.github.com/repos/test/arc-node/releases/latest)
        if [[ "$headers" == *"Authorization:"* ]]; then
            printf 'curl verbose Authorization: Bearer stale-token\n' >&2
        else
            printf 'anonymous failed\n' >&2
        fi
        exit 22
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac
EOF
    chmod 755 "$fakebin/curl"

    if (
        PATH="$fakebin:$PATH"
        GITHUB_AUTH_TOKEN="stale-token"
        CURL_HEADERS=()
        REPO="test/arc-node"
        GITHUB_API_URL="https://api.github.com"
        get_latest_version
    ) >"$out" 2>&1; then
        cat "$out" >&2
        fail "latest version redacts authenticated failure"
    fi

    if grep -q "stale-token" "$out" || grep -q "Authorization: Bearer" "$out"; then
        cat "$out" >&2
        fail "latest version redacts authenticated failure"
    fi
    if ! grep -q "request failed" "$out"; then
        cat "$out" >&2
        fail "latest version redacts authenticated failure"
    fi
    pass "latest version redacts authenticated failure"
}

test_download_file_uses_gh_when_available() {
    local fakebin="$TEST_TMP/gh-download-fakebin"
    local release_dir="$TEST_TMP/gh-download-release"
    local output_dir="$TEST_TMP/gh-download-output"
    local asset_name="arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"

    mkdir -p "$fakebin" "$release_dir" "$output_dir"
    printf 'release asset\n' > "$release_dir/$asset_name"

    cat > "$fakebin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" != "release" || "${2:-}" != "download" ]]; then
    exit 1
fi
shift 2

tag="$1"
shift
repo=""
pattern=""
dir=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo)
            repo="$2"
            shift 2
            ;;
        --pattern)
            pattern="$2"
            shift 2
            ;;
        --dir)
            dir="$2"
            shift 2
            ;;
        --clobber)
            shift
            ;;
        *)
            exit 1
            ;;
    esac
done

[[ "$tag" == "v1.2.3" ]]
[[ "$repo" == "test/arc-node" ]]
cp "$GH_RELEASE_DIR/$pattern" "$dir/$pattern"
EOF
    chmod 755 "$fakebin/gh"

    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
echo "curl should not be called when gh succeeds" >&2
exit 99
EOF
    chmod 755 "$fakebin/curl"

    (
        PATH="$fakebin:$PATH"
        export GH_RELEASE_DIR="$release_dir"
        GITHUB_AUTH_TOKEN=""
        CURL_HEADERS=()
        REPO="test/arc-node"
        download_file "v1.2.3" "$asset_name" "$output_dir"
    ) >"$TEST_TMP/gh-download.out" 2>&1

    if [[ "$(cat "$output_dir/$asset_name")" != "release asset" ]]; then
        cat "$TEST_TMP/gh-download.out" >&2
        fail "download_file uses gh when available"
    fi
    pass "download_file uses gh when available"
}

test_download_file_falls_back_to_curl() {
    local fakebin="$TEST_TMP/curl-fallback-fakebin"
    local release_dir="$TEST_TMP/curl-fallback-release"
    local output_dir="$TEST_TMP/curl-fallback-output"
    local asset_name="arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"

    mkdir -p "$fakebin" "$release_dir" "$output_dir"
    printf 'fallback asset\n' > "$release_dir/$asset_name"

    cat > "$fakebin/gh" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
    chmod 755 "$fakebin/gh"

    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

out=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o)
            out="$2"
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

case "$url" in
    *releases/download/v1.2.3/arc-node-v1.2.3-aarch64-apple-darwin.tar.gz)
        cp "$FALLBACK_RELEASE_DIR/${url##*/}" "$out"
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac
EOF
    chmod 755 "$fakebin/curl"

    (
        PATH="$fakebin:$PATH"
        export FALLBACK_RELEASE_DIR="$release_dir"
        GITHUB_AUTH_TOKEN=""
        CURL_HEADERS=()
        REPO="test/arc-node"
        download_file "v1.2.3" "$asset_name" "$output_dir"
    ) >"$TEST_TMP/curl-fallback.out" 2>&1

    if [[ "$(cat "$output_dir/$asset_name")" != "fallback asset" ]]; then
        cat "$TEST_TMP/curl-fallback.out" >&2
        fail "download_file falls back to curl"
    fi
    pass "download_file falls back to curl"
}

test_fixture_install_matrix() {
    local fixture_dir="$TEST_TMP/fixture"
    local fakebin="$TEST_TMP/fakebin"
    local release_dir="$fixture_dir/release"

    mkdir -p "$fixture_dir/build" "$release_dir" "$fakebin"
    for binary in "${BINARIES[@]}"; do
        printf '#!/usr/bin/env bash\nprintf "%s test binary\\n"\n' "$binary" > "$fixture_dir/build/$binary"
        chmod 755 "$fixture_dir/build/$binary"
    done

    local target archive_name archive checksum_file
    for target in \
        "x86_64-unknown-linux-gnu" \
        "aarch64-unknown-linux-gnu" \
        "aarch64-apple-darwin"; do
        archive_name="arc-node-v1.2.3-${target}.tar.gz"
        archive="$release_dir/$archive_name"
        checksum_file="$release_dir/$archive_name.sha256"
        tar -czf "$archive" -C "$fixture_dir/build" "${BINARIES[@]}"
        printf '%s  %s\n' "$(compute_sha256 "$archive")" "$archive_name" > "$checksum_file"
    done

    cat > "$fakebin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
    -s) echo "$FAKE_UNAME_S" ;;
    -m) echo "$FAKE_UNAME_M" ;;
    *) /usr/bin/uname "$@" ;;
esac
EOF
    chmod 755 "$fakebin/uname"

    cat > "$fakebin/gh" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
    chmod 755 "$fakebin/gh"

    cat > "$fakebin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

out=""
url=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o)
            out="$2"
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        -*)
            shift
            ;;
        *)
            url="$1"
            shift
            ;;
    esac
done

case "$url" in
    *raw.githubusercontent.com/circlefin/arc-node/main/arcup/arcup)
        data='ARCUP_INSTALLER_VERSION="0.2.0"'
        ;;
    *releases/download/v1.2.3/arc-node-v1.2.3-*.tar.gz*)
        name="${url##*/}"
        cp "$FIXTURE_RELEASE_DIR/$name" "$out"
        exit 0
        ;;
    *api.github.com/repos/test/arc-node/releases/tags/v1.2.3)
        data='{"assets":[{"name":"arc-node-v1.2.3-x86_64-unknown-linux-gnu.tar.gz"},{"name":"arc-node-v1.2.3-aarch64-unknown-linux-gnu.tar.gz"},{"name":"arc-node-v1.2.3-aarch64-apple-darwin.tar.gz"}]}'
        ;;
    *)
        printf 'unexpected curl URL: %s\n' "$url" >&2
        exit 22
        ;;
esac

if [[ -n "$out" ]]; then
    printf '%s\n' "$data" > "$out"
else
    printf '%s\n' "$data"
fi
EOF
    chmod 755 "$fakebin/curl"

    local case_name os arch install_dir
    while read -r case_name os arch target; do
        install_dir="$TEST_TMP/install-$case_name"
        mkdir -p "$install_dir"
        FIXTURE_RELEASE_DIR="$release_dir" \
        FAKE_UNAME_S="$os" \
        FAKE_UNAME_M="$arch" \
        ARC_GITHUB_TOKEN="" \
        GH_TOKEN="" \
        GITHUB_TOKEN="" \
        PATH="$fakebin:$PATH" \
        ARC_REPO="test/arc-node" \
        ARC_BIN_DIR="$install_dir/bin" \
        "$ROOT_DIR/arcup/arcup" -i "1.2.3" >"$TEST_TMP/install-$case_name.out" 2>&1

        for binary in "${BINARIES[@]}"; do
            [[ -x "$install_dir/bin/$binary" ]] || fail "installs $binary for $target"
        done
    done <<'EOF'
linux-amd64 Linux x86_64 x86_64-unknown-linux-gnu
linux-arm64 Linux aarch64 aarch64-unknown-linux-gnu
macos-arm64 Darwin arm64 aarch64-apple-darwin
EOF

    pass "fixture install covers all release targets"
}

test_archive_path_traversal_fails() {
    local bad_archive="$TEST_TMP/bad.tar.gz"
    local bad_src="$TEST_TMP/bad-src"

    mkdir -p "$bad_src"
    printf 'bad\n' > "$bad_src/evil"
    if tar --version 2>/dev/null | grep -q "GNU tar"; then
        tar -czf "$bad_archive" -C "$bad_src" --transform 's|evil|../evil|' evil
    else
        tar -czf "$bad_archive" -C "$bad_src" -s '|evil|../evil|' evil
    fi

    if ( validate_archive_contents "$bad_archive" ) >"$TEST_TMP/path_traversal.out" 2>&1; then
        cat "$TEST_TMP/path_traversal.out" >&2
        fail "archive path traversal fails"
    fi
    pass "archive path traversal fails"
}

test_archive_link_entries_fail() {
    local bad_archive="$TEST_TMP/link-entry.tar.gz"
    local bad_src="$TEST_TMP/link-entry-src"

    mkdir -p "$bad_src"
    ln -s /etc/passwd "$bad_src/arc-node-execution"
    tar -czf "$bad_archive" -C "$bad_src" arc-node-execution

    if ( validate_archive_contents "$bad_archive" ) >"$TEST_TMP/link-entry.out" 2>&1; then
        cat "$TEST_TMP/link-entry.out" >&2
        fail "archive link entries fail"
    fi
    pass "archive link entries fail"
}

test_install_binary_rejects_symlink() {
    local install_src="$TEST_TMP/install-symlink-src"
    local install_bin="$TEST_TMP/install-symlink-bin"

    mkdir -p "$install_src" "$install_bin"
    ln -s /etc/passwd "$install_src/arc-node-execution"

    if (
        TMP_DIR="$install_src"
        BIN_DIR="$install_bin"
        install_binary "arc-node-execution"
    ) >"$TEST_TMP/install-symlink.out" 2>&1; then
        cat "$TEST_TMP/install-symlink.out" >&2
        fail "install_binary rejects symlink"
    fi
    pass "install_binary rejects symlink"
}

test_version_normalization
test_version_comparison
test_target_mapping
test_unknown_architecture_fails
test_github_api_url_rejects_non_https
test_checksum_validation
test_download_error_lists_assets
test_download_file_uses_github_token_api
test_token_api_preserves_custom_header
test_latest_version_retries_anonymous_after_token_failure
test_latest_version_redacts_authenticated_failure
test_download_file_uses_gh_when_available
test_download_file_falls_back_to_curl
test_fixture_install_matrix
test_archive_path_traversal_fails
test_archive_link_entries_fail
test_install_binary_rejects_symlink
