# Multi-stage Dockerfile to build a minimal, lightweight image with `spammer`
# CLI for linux/amd64 targets, such as Amazon Linux 2023 images used by Quake in
# remote deployments.
#
# Build with:
#   docker build --build-arg CIRCLE_BASE_IMAGE=$CIRCLE_BASE_IMAGE --platform linux/amd64 -f crates/spammer/Dockerfile -t spammer:latest .

###############
# Base
###############
ARG CIRCLE_BASE_IMAGE
FROM ${CIRCLE_BASE_IMAGE} AS circle-base

##########
# Builder
##########
FROM rust:1-bookworm AS builder

# Define build-time arguments.
ARG RUSTFLAGS=""

# Set environment variables for the build process.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_HTTP_CAINFO=/etc/ssl/certs/ca-certificates.crt
ENV RUSTFLAGS="${RUSTFLAGS}"

# Copy SSL configuration from ssl stage
COPY --from=circle-base /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

WORKDIR /app

# Copy only the minimal set of files needed to build Spammer and its internal deps
COPY Cargo.toml Cargo.lock ./
COPY crates/spammer/ crates/spammer/
COPY crates/version/ crates/version/

# Patch Cargo.toml for this minimal build context:
# 1. Remove [patch.crates-io] — requires git auth, not needed by spammer.
# 2. Simplify workspace members to just "crates/*" — the full workspace lists
#    crates/test/* explicitly, but those aren't copied into the image.
# 3. Remove the exclude line — only needed alongside those explicit members.
RUN sed -i \
    -e '/\[patch\.crates-io\]/,/^$/d' \
    -e 's|members = \[.*\]|members = ["crates/*"]|' \
    -e '/^\[workspace\]$/,/^\[/{/^exclude/d}' \
    Cargo.toml

# Ensure we don't reuse old target artifacts built against OpenSSL 1.1.
RUN cargo clean

# Use BuildKit cache mounts to speed up subsequent builds by caching
# the Cargo registry, git dependencies, and the entire build target directory.
RUN --mount=type=cache,sharing=private,target=/usr/local/cargo/registry \
	--mount=type=cache,sharing=private,target=/usr/local/cargo/git \
    # Build the Spammer binary.
    cargo build -p spammer --release && \
    # Copy the built binary to a temporary, non-cached location to make it accessible to the next stage.
    cp /app/target/release/spammer /tmp/spammer

###########
# Runtime
###########
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
	ca-certificates \
	libssl3 \
	tini \
	&& rm -rf /var/lib/apt/lists/*

# Copy the application binary from the builder stage into the final image.
COPY --from=builder /tmp/spammer /usr/local/bin/spammer

# Running containers as a privileged user is security-sensitive (SonarQube docker:S6471)
RUN addgroup --system nonroot && adduser --system --ingroup nonroot nonroot
USER nonroot

# Use tini as PID 1 to correctly handle and forward signals (e.g., Ctrl-C)
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/spammer"]
CMD ["--help"]
