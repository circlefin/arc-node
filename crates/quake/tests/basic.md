# Testnet lifecycle

<!-- md-exec: abort_on_fail, print_command_output -->

> [!NOTE] Executable documentation
>
> The code snippets in this file are executed and validated by `scripts/md-exec.py`.

This test verifies the basic Quake commands: `setup`, `build`, `start`, `stop`, and `clean`.

## Prepare for deployment

#### Build Quake
```sh
$ cargo build -p quake
...Finished...
```

#### Create a temporary manifest file with five validators
```sh {empty_output}
$ export MANIFEST=/tmp/quake-test-basic.toml
$ cat > $MANIFEST << 'EOF'
[nodes.validator1]
[nodes.validator2]
[nodes.validator3]
[nodes.validator4]
[nodes.validator5]
EOF
```

<details>
<summary>Clean up any previous testnet state</summary>

Remove any existing containers and testnet files:
```sh
$ ./target/debug/quake -f $MANIFEST clean --all
...✅ Testnet cleaned
```

Verify all files were removed:
```sh
$ ls .quake/quake-test-basic
... No such file or directory
```
</details>

<details>
<summary>Verify the last used manifest file was updated</summary>

```sh {strip}
$ cat .quake/.last_manifest
/tmp/quake-test-basic.toml
```
</details>

## Setup

#### Generate the configuration files required to run the testnet
```sh
$ ./target/debug/quake setup
...✅ Testnet setup completed...
```

<details>
<summary>Verify the main files were created</summary>

```sh
$ ls .quake/quake-test-basic/compose.yaml .quake/quake-test-basic/nodes.json .quake/quake-test-basic/assets/genesis.json
.quake/quake-test-basic/assets/genesis.json
.quake/quake-test-basic/compose.yaml
.quake/quake-test-basic/nodes.json
```
</details>

#### Build the Docker images
```sh {timeout=1800}
$ time ./target/debug/quake -vv build
...✅ Docker images built...
```

<details>
<summary>Verify the images were created</summary>

```sh
$ docker images --format '{{.Repository}}:{{.Tag}}' | grep -E '^arc_consensus|arc_execution' | sort
arc_consensus:latest
arc_execution:latest
```
</details>

## Start the testnet
```sh {timeout=300}
$ ./target/debug/quake -v start
...✅ Testnet started...
```

<details>
<summary>Verify logs are being produced</summary>

```sh
$ ls .quake/quake-test-basic/logs/*.log | wc -l | tr -d ' '
10
```
</details>

#### Wait for initial block production
```sh
$ ./target/debug/quake wait height 5
...✅ Nodes have reached height 5...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```

## Stop the testnet

#### Stop all nodes (monitoring services remain running)
```sh
$ ./target/debug/quake stop
...✅ Testnet stopped...
```

<details>
<summary>Verify monitoring services are still running</summary>

```sh
$ docker ps --format '{{.Names}}' | grep -E '^prometheus|^grafana' | sort
grafana
prometheus
```
</details>

## Clean up

#### Stop all containers (including monitoring) and remove all testnet files
```sh {timeout=60}
$ ./target/debug/quake -v clean --all
...✅ Testnet is down...
...✅ Testnet data removed...
...✅ Monitoring data removed...
...✅ Testnet cleaned
```

<details>
<summary>Verify no containers are running</summary>

```sh {empty_output}
$ docker ps -a --format '{{.Names}}' | grep -E '^validator|^full|^prometheus|^grafana'
```
</details>

## Restart the testnet

#### Verify the previous `clean` command worked by starting a fresh testnet
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

## Finally, clean up

<details>
<summary>Stop the testnet and remove all data</summary>

```sh {timeout=120}
$ ./target/debug/quake -v clean --all
...✅ Testnet is down...
...✅ Testnet data removed...
...✅ Monitoring data removed...
...✅ Testnet cleaned
```
</details>
