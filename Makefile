# Test configuration
UNIT_TEST_ARGS := --locked --workspace
COV_FILE := target/lcov.info
SCRIPTS="./scripts"
FOUNDRY_VERSION := $(shell cat .foundry-version)
QUAKE_MANIFEST ?= crates/quake/scenarios/localdev.toml
NUM_VALIDATORS := $(shell grep -c '^\[nodes\.validator' $(QUAKE_MANIFEST) 2>/dev/null || echo 5)
QUAKE := cargo run --bin quake --
LOAD_PREDEFINED_ARC_REMOTE_SIGNER_KEYS := true
DEFAULT_BRANCH ?= $(shell git symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's@^refs/remotes/origin/@@')
ifeq ($(DEFAULT_BRANCH),)
DEFAULT_BRANCH = master
endif

##@ Help
.PHONY: help
help: ## Display this help message
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n"} /^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2 } /^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) } ' $(MAKEFILE_LIST)

.DEFAULT_GOAL := help

##@ Development
.PHONY: check-foundry
check-foundry: ## Check Foundry version
	@INSTALLED=$$(forge --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1); \
	REQUIRED=$(FOUNDRY_VERSION:v%=%); \
	if [ "$$INSTALLED" != "$$REQUIRED" ]; then \
		echo "Error: Foundry $(FOUNDRY_VERSION) required. Run: foundryup -i $(FOUNDRY_VERSION)"; \
		exit 1; \
	fi

.PHONY: fmt
fmt: ## Format code using rustfmt
	cargo fmt
	npx prettier --config ./.prettierrc --write '**/*.{ts,js,mts,mjs}'

.PHONY: clippy
clippy: ## Run clippy lints
	cargo clippy \
		--workspace \
		--lib \
		--examples \
		--tests \
		--benches \
		--all-features \
		-- -D warnings

.PHONY: buf-lint
buf-lint: ## Lint protobuf files using buf
	buf lint

.PHONY: buf-format
buf-format: ## Format protobuf files using buf
	buf format -w

.PHONY: buf-breaking
buf-breaking: ## Check for breaking changes in protobuf files
	buf breaking --against '.git#branch=$(DEFAULT_BRANCH)'

.PHONY: lint
lint: fmt buf-format build-contract ## Run formatting and linting
	$(MAKE) clippy
	npx eslint
	! grep -R 'it.only' tests/ -q # check there are no it.only in testing scripts

##@ Build
.PHONY: build
build: ## Build the reth binary into `target` directory
	@echo building...
	cargo build

.PHONY: build-quake
build-quake: ## Build quake binary
	cargo build --bin quake

.PHONY: build-docker
build-docker: ## Build Docker images for integration stack
	@echo building docker images...
	BUILD_PROFILE=dev $(SCRIPTS)/build-docker.sh

HARDHAT = npx hardhat --config hardhat.config.ts

.PHONY: build-contract
build-contract: check-foundry ## Build the contracts and bindings
	npm install
	$(HARDHAT) compile

genesis: build-contract  ## Generate the genesis file idempotently
	$(HARDHAT) genesis --network localdev --num-validators $(NUM_VALIDATORS)

.PHONY: mine-denylist-salt
mine-denylist-salt: check-foundry ## Mine a CREATE2 salt for the Denylist proxy with a 0x360 address prefix (usage: make mine-denylist-salt INIT_CODE_HASH=0x...)
	@test -n "$$INIT_CODE_HASH" || { echo "Error: INIT_CODE_HASH is required"; exit 1; }
	cast create2 \
		--starts-with 360 \
		--init-code-hash $$INIT_CODE_HASH \
		--seed "$$(printf 'Denylist.v1' | cast keccak)"

.PHONY: clean
clean: ## Clean contract artifacts files and caches
	npx hardhat clean
	forge clean
	$(RM) contracts/cache/storage-code*.json

##@ Local Environment
.PHONY: up down dev

up: genesis ## Start integration services for full local integration
	@echo setting up env...
	$(SCRIPTS)/up.sh

down: ## Stop integration services (set CLEAN=true to drop volumes)
	@echo tearing down env...
	$(SCRIPTS)/down.sh

dev: genesis ## Run main project in dev mode
	./scripts/localdev.mjs stop clean start $(LAUNCH_ARGS)

##@ Test
.PHONY: test-unit
test-unit: build genesis ## Run unit tests with linting
	@echo running linting and unit tests...
	make lint
	cargo install cargo-nextest --locked
	cargo nextest run $(UNIT_TEST_ARGS)

.PHONY: test-it
test-it: up ## Run integration tests
	@echo running linting, unit tests, and integration tests...
	make lint
	cargo install cargo-nextest --locked
	cargo nextest run $(UNIT_TEST_ARGS) --features integration

.PHONY: test-all
test-all: test-it test-unit-contract ## Run all tests
	@echo running all tests...
	make smoke LAUNCH_ARGS="--frozen --healthy-retry=130"

.PHONY: cov-unit
cov-unit: genesis ## Run unit tests with coverage
	rm -f $(COV_FILE)
	cargo llvm-cov nextest --lcov --output-path $(COV_FILE) $(UNIT_TEST_ARGS) --exclude-from-report arc-execution-e2e

.PHONY: cov-it
cov-it: up ## Run integration tests with coverage
	rm -f $(COV_FILE)
	cargo llvm-cov nextest --lcov --output-path $(COV_FILE) $(UNIT_TEST_ARGS) --features integration --exclude-from-report arc-execution-e2e

.PHONY: cov-report
cov-report: cov-unit ## Generate the coverage report
	cargo llvm-cov report --html

.PHONY: cov-show
cov-show: cov-report ## Generate coverage report and open in browser
	open target/llvm-cov/html/index.html

.PHONY: test-unit-contract
test-unit-contract: check-foundry ## Run contract unit tests with coverage
	@echo "Running contract tests..."
	@forge test -vvv
	@echo ""
	@echo "Coverage Summary:"
	@echo "================"
	@forge coverage --report summary --offline 2>&1 | \
		grep -E "(protocol-config|validator-manager|AdminUpgradeableProxy|^\| Total)" | \
		grep -v "interfaces" | \
		grep -v "contracts/test"

.PHONY: test-unit-hardhat
test-unit-hardhat: ## Run hardhat unit tests
	npx hardhat test ./tests/helpers/matchers/index.test.ts ./tests/unit/*.test.ts --no-compile

.PHONY: test-localdev
test-localdev: ## Run hardhat localdev tests
	$(HARDHAT) test ./tests/localdev/*.test.ts --network localdev

.PHONY: test-simulation
test-simulation: ## Run hardhat simulation tests
	$(HARDHAT) test ./tests/simulation/*.test.ts --network localdev

.PHONY: smoke
smoke: genesis ## Run smoke tests (both reth and malachite)
	@echo "Running smoke tests for both reth and malachite..."
	@echo "Step 1/2: Running smoke-reth tests..."
	$(MAKE) smoke-reth
	@echo "smoke-reth completed"
	@echo "Step 2/2: Running smoke-malachite tests..."
	$(MAKE) smoke-malachite
	@echo "smoke-malachite completed"
	@echo "All smoke tests completed successfully!"

.PHONY: smoke-reth
smoke-reth: genesis ## Run Reth smoke tests
	@echo "Running smoke tests on local reth(mock CL)..."
	@bash -c '\
		set -ex; \
		trap "./scripts/localdev.mjs stop --network=localdev" EXIT; \
		./scripts/localdev.mjs stop clean daemon --network=localdev $(LAUNCH_ARGS); \
		$(MAKE) test-localdev; \
		$(MAKE) test-simulation; \
	'

.PHONY: smoke-malachite
smoke-malachite: testnet ## Run Malachite smoke & Quake tests
	@echo "Running smoke tests on local reth + malachite using testnet setup..."
	@bash -c '\
		set -ex; \
		trap "$(MAKE) testnet-clean" EXIT; \
		env LOAD_PREDEFINED_ARC_REMOTE_SIGNER_KEYS=$(LOAD_PREDEFINED_ARC_REMOTE_SIGNER_KEYS) $(MAKE) test-localdev; \
	'

.PHONY: smoke-quake
smoke-quake: testnet
	@echo "Running quake tests against local network..."
	$(MAKE) testnet-test

##@ Testnet
.PHONY: testnet
testnet: genesis build-docker ## Start testnet as defined in QUAKE_MANIFEST file
	@echo "Setting up and starting Quake testnet..."
	$(QUAKE) -f $(QUAKE_MANIFEST) start

.PHONY: testnet-test
testnet-test: ## Run tests against running testnet
	$(QUAKE) test

.PHONY: testnet-down
testnet-down: ## Stop testnet
	$(QUAKE) stop

.PHONY: testnet-clean
testnet-clean: ## Remove testnet artifacts
	# Remove Docker Compose build containers that don't get cleaned up by 'docker compose down'
	# These are transient containers created during image builds (e.g., testnet-arc_execution_build-1)
	@docker ps -a --format "{{.ID}} {{.Names}}" | grep -E "arc_execution_build|arc_consensus_build" | awk '{print $$1}' | xargs -r docker rm -f 2>/dev/null || true
	$(QUAKE) -f $(QUAKE_MANIFEST) clean --all
	# Clean up any stale testnet networks to prevent IP conflicts on next run
	@docker network ls --format "{{.Name}}" | grep -E "^localdev_" | xargs -r docker network rm 2>/dev/null || true

.PHONY: testnet-load
testnet-load: ## Send tx load to testnet (backpressure) (usage: make testnet-load RATE=1000 TIME=60)
	@RATE=$${RATE:-1000}; \
	TIME=$${TIME:-60}; \
	echo "Sending $$RATE TPS for $$TIME seconds to testnet..."; \
	$(QUAKE) -f $(QUAKE_MANIFEST) load -r $$RATE -t $$TIME

.PHONY: testnet-load-stop
testnet-load-stop: ## Stop all running load processes
	@pkill -f "quake.*(load|spam)" || true
	@echo "Load processes stopped (if any were running)"

.PHONY: quake-test
quake-test: ## Run e2e tests for Quake
	python3 scripts/md-exec.py crates/quake/tests
