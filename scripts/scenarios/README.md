# Quake Test Scenarios

This directory contains test quake scenario scripts that can be run locally or in CI/CD pipelines.

Each scenario consists of three components:
- **Script:** `scripts/scenarios/<name>.sh` - Bash script that runs the test
- **Config:** `crates/quake/scenarios/<name>.toml` - Quake scenario configuration
- **Workflow:** `.github/workflows/<name>.yml` - GitHub Actions automation

Scripts should automatically build, test, collect results to `target/test-results/`, and clean up.

GitHub Actions workflows run on schedule or can be manually triggered with configurable parameters.

## Available Scenarios

### nightly-upgrade

Tests rolling upgrades under transaction load. Validates that nodes can be upgraded one-by-one without disrupting consensus or transaction processing.

```bash
./scripts/scenarios/nightly-upgrade.sh [scenario] [load_duration] [load_rate]
```

- Tests parallel load and rolling upgrades
- Maintains consensus with 4/5 validators during upgrades
- Verifies nodes sync after upgrade
- Measures transaction throughput during disruption

Results saved to `target/test-results/`: `load_results.txt`, `upgrade_results.txt`, `final_heights.txt`

### nightly-chaos-testing

Tests network resilience under chaos (random kill/pause/restart and valset changes) with transaction load.

```bash
./scripts/scenarios/nightly-chaos-testing.sh [scenario] [spam_duration_seconds] [tx_rate]
```

- Default scenario: `crates/quake/scenarios/nightly-chaos-testing.toml`
- Runs spammer and chaos loop in parallel; results in `target/nightly-chaos-testing-results/`

## Adding New Scenarios

1. Create or reuse a TOML config in `crates/quake/scenarios/<name>.toml`
2. Create a bash script in `scripts/scenarios/<name>.sh` (see `nightly-upgrade.sh` as example)
3. Create a GitHub workflow in `.github/workflows/<name>.yml` (see `nightly-upgrade.yml` as example)
4. Document the scenario in this README

**Script requirements:**
- Save results to `target/test-results/`
- Exit with 0 on success, non-zero on failure
- Clean up Docker resources

## Potential Future Scenarios

- **smoketest** - Run the smoke tests, with parameters, against various scenarios
- **network-partition** - Test consensus recovery after network splits
- **state-sync** - Test new nodes syncing from scratch or snapshots
- **gas-price-volatility** - Test under varying gas price conditions
- **reorg-resilience** - Test handling of chain reorganizations
- **validator-churn** - Test frequent validator set changes
- **byzantine-node** - Test behavior with malicious/faulty validators
