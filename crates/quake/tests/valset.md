# Validator set updates

<!-- md-exec: abort_on_fail, print_command_output -->

> [!NOTE] Executable documentation
>
> The code snippets in this file are executed and validated by `scripts/md-exec.py`.

This test verifies that the `quake valset` command correctly updates validator voting power.

## Initialize the testnet

<details>
<summary>Prepare for deployment</summary>

Build Quake:
```sh
$ cargo build -p quake
...Finished...
```

Create a temporary manifest file with validators and full nodes:
```sh {empty_output}
$ export MANIFEST=/tmp/quake-test-valset.toml
$ cat > $MANIFEST << 'EOF'
[nodes.validator1]
[nodes.validator2]
[nodes.validator-blue]
[nodes.validator-yellow]
[nodes.full1]
[nodes.full2]
[nodes.full3]
EOF
```

Clean up any previous testnet state:
```sh
$ ./target/debug/quake -f $MANIFEST clean --all
...✅ Testnet cleaned
```
</details>


<details>
<summary>Start the testnet and wait for block production</summary>

```sh {timeout=300}
$ ./target/debug/quake -v start
...✅ Testnet started...
```

Wait for initial block production:
```sh
$ ./target/debug/quake wait height 5
...✅ Nodes have reached height 5...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
</details>

### Verify all validators start with the default voting power
```sh {strip}
$ ./target/debug/quake info
...
* Validator set:
...validator1...Voting Power: 20...
...validator2...Voting Power: 20...
...validator-blue...Voting Power: 20...
...validator-yellow...Voting Power: 20...
```

## Update a single validator's voting power

#### Update the voting power of validator1
```sh
$ ./target/debug/quake valset validator1:100
...✅ Voting power updated...
```

<details>
<summary>Wait for the voting power update to take effect</summary>

```sh
$ ./target/debug/quake wait height 10
...✅ Nodes have reached height 10...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
Waiting for additional blocks ensures the validator set contract update is applied.
</details>

#### Verify validator1's new voting power
```sh
$ ./target/debug/quake info
...
* Validator set:
...validator1...Voting Power: 100...
...validator2...Voting Power: 20...
...validator-blue...Voting Power: 20...
...validator-yellow...Voting Power: 20...
```

## Update multiple validators at once

#### Update the voting power of validator1 and validator2
```sh
$ ./target/debug/quake valset validator1:150 validator2:200
...✅ Voting power updated...
...✅ Voting power updated...
```

<details>
<summary>Wait for the updates to take effect</summary>

```sh
$ ./target/debug/quake wait height 20
...✅ Nodes have reached height 20...
$ ./target/debug/quake wait sync
...✅ Nodes have finished syncing...
```
</details>

#### Verify both validators have updated voting power
```sh
$ ./target/debug/quake info
...
* Validator set:
...validator1...Voting Power: 150...
...validator2...Voting Power: 200...
...validator-blue...Voting Power: 20...
...validator-yellow...Voting Power: 20...
```

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
