# Node upgrades

<!-- md-exec: abort_on_fail, print_command_output -->

> [!NOTE] Executable documentation
>
> The code snippets in this file are executed and validated by `scripts/md-exec.py`.

This test verifies that the `quake perturb upgrade` command correctly upgrades nodes to a new version.

## Initialize the testnet

Create a temporary manifest file with three validators:
```sh {empty_output}
$ export MANIFEST=/tmp/quake-test-upgrade.toml
$ cat > $MANIFEST << 'EOF'
engine_api_connection = "rpc"
el_init_hardfork="zero4"

# IMAGE_REGISTRY_URL should be defined as an environment variable or in the .env file.
image_cl="${IMAGE_REGISTRY_URL}/arc-consensus:latest"
image_el="${IMAGE_REGISTRY_URL}/arc-execution:latest"

image_cl_upgrade="arc_consensus:latest"
image_el_upgrade="arc_execution:latest"

[nodes.validator1]
[nodes.validator2]
[nodes.validator3]
EOF
```

<details>
<summary>Prepare for deployment</summary>

Build Quake:
```sh
$ cargo build -p quake
...Finished...
```

Clean up any previous testnet state:
```sh
$ ./target/debug/quake -f $MANIFEST clean --all
...✅ Testnet cleaned
```
</details>

#### Set up testnet files
```sh
$ ./target/debug/quake setup
...✅ Testnet setup completed...
```

#### Build the Docker images, if needed
```sh {timeout=1800}
$ time ./target/debug/quake -vv build
...✅ Docker images built...
```

#### Verify the images were pulled
```sh {strip}
$ docker images --format '{{.Repository}}:{{.Tag}}' | grep -E 'ghcr.io.*arc-consensus|ghcr.io.*arc-execution' | sort
...
$ docker images --format '{{.Repository}}:{{.Tag}}' | grep -E '^arc_consensus|^arc_execution' | sort
...
arc_consensus:latest
arc_execution:latest
...
```

#### Start the testnet
```sh {timeout=300}
$ ./target/debug/quake -v start
...✅ Testnet started...
```

<details>
<summary>Wait for block production</summary>

```sh
$ ./target/debug/quake wait height 5
...✅ Nodes have reached height 5...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
</details>

## Upgrade a single node

#### Upgrade validator1
```sh
$ ./target/debug/quake perturb upgrade validator1
...✅ Perturbation applied...
```

#### Verify validator1 is now running the upgraded containers (with `_u` suffix)
```sh
$ docker ps --format '{{.Names}}' | grep validator1 | sort
validator1_cl_u
validator1_el_u
```

<details>
<summary>Verify the testnet continues producing blocks</summary>

```sh
$ ./target/debug/quake wait height 10
...✅ Nodes have reached height 10...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
</details>

## Upgrade multiple nodes at once

#### Upgrade validator2 and validator3
```sh
$ ./target/debug/quake perturb upgrade validator2 validator3
...✅ Perturbation applied...
```

#### Verify validator2 and validator3 are now running the upgraded containers (with `_u` suffix)
```sh
$ docker ps --format '{{.Names}}' | grep -E '^validator2|^validator3' | sort
validator2_cl_u
validator2_el_u
validator3_cl_u
validator3_el_u
```

<details>
<summary>Verify the testnet continues producing blocks</summary>

```sh
$ ./target/debug/quake wait height 15
...✅ Nodes have reached height 15...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
</details>

## Finally, clean up

<details>
<summary>Stop the testnet and remove all data</summary>

```sh {timeout=60}
$ ./target/debug/quake -v clean --all
...✅ Testnet is down...
...✅ Testnet data removed...
...✅ Monitoring data removed...
...✅ Testnet cleaned
```
</details>
