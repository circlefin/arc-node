# Quake end-to-end tests

This directory contains end-to-end tests for Quake written as executable Markdown documentation.

Tests are executed using the `md-exec.py` script, which parses the Markdown files, extracts shell commands, runs them, and validates the output.

## Test Files

| File | Description |
|------|-------------|
| [basic.md](basic.md) | Tests the basic Quake lifecycle: `setup`, `build`, `start`, `stop`, and `clean` |
| [subnets.md](subnets.md) | Tests nodes assigned to multiple subnets |
| [upgrade.md](upgrade.md) | Tests the `perturb upgrade` command for upgrading nodes |
| [valset.md](valset.md) | Tests the `valset` command for updating validator voting power |
