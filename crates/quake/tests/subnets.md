# Multiple subnets

<!-- md-exec: abort_on_fail, print_command_output -->

> [!NOTE] Executable documentation
>
> The code snippets in this file are executed and validated by `scripts/md-exec.py`.

This test verifies that nodes can be assigned to multiple subnets and communicate correctly within each subnet.

## Manifest

For the test we create a temporary manifest file with validators in different subnets:
```sh {empty_output}
$ export MANIFEST=/tmp/quake-test-subnets.toml
$ cat > $MANIFEST << 'EOF'
[nodes.validator1]
subnets = ["trusted"]
[nodes.validator2]
subnets = ["trusted"]
[nodes.validator3]
subnets = ["trusted", "untrusted"]
[nodes.validator4]
subnets = ["untrusted", "default"]
[nodes.full1]
EOF
```

## Initialize the testnet

<details>
<summary>Prepare for deployment</summary>

Build Quake:
```sh
$ cargo build -p quake
...Finished...
```

Clean up any running testnet:
```sh
$ quake clean --all
...✅ Testnet cleaned
```

Add quake to PATH for convenience:
```sh
$ export PATH=$PATH:./target/debug/
```
</details>


## Start the testnet

```sh {timeout=300}
$ quake -v -f $MANIFEST start
...✅ Testnet started...
```

<details>
<summary>Wait for initial block production</summary>

```sh
$ quake wait height 10
...✅ Nodes have reached height 10...
$ quake wait sync
...✅ Nodes have finished syncing...
```
</details>

## Test network isolation

To verify that subnet isolation works correctly, we stop the bridge node
(`validator4`) that connects the `untrusted` and `default` subnets. Once
stopped, `full1` (which is only in the `default` subnet) should stop receiving
new blocks since its only path to validators is through `validator4`. Other
validators should continue operating normally.

Stop validator4:
```sh
$ quake stop validator4
...✅ Testnet stopped containers=validator4
```

Record full1's current height, wait a few seconds, and check that full1's height is stuck:
```sh
$ export FULL1_HEIGHT=$(quake -q info height full1 | tr -d '[:space:]')
$ echo "full1 height after stopping validator4: $FULL1_HEIGHT"
full1 height...
$ sleep 5
$ export FULL1_HEIGHT_AFTER=$(quake -q info height full1 | tr -d '[:space:]')
$ echo "full1 height after disconnect: $FULL1_HEIGHT_AFTER"
full1 height...
$ if [ "$FULL1_HEIGHT" -eq "$FULL1_HEIGHT_AFTER" ]; then echo "✅ full1 is isolated (height unchanged)"; else echo "❌ full1 is NOT isolated (height changed from $FULL1_HEIGHT to $FULL1_HEIGHT_AFTER)"; exit 1; fi
✅ full1 is isolated (height unchanged)
```

Restart validator4 and verify full1 starts syncing again:
```sh
$ quake start validator4
...✅ Testnet started...
$ sleep 5
$ export FULL1_HEIGHT_RECONNECT=$(quake -q info height full1 | tr -d '[:space:]')
$ if [ "$FULL1_HEIGHT_RECONNECT" -gt "$FULL1_HEIGHT_AFTER" ]; then echo "✅ full1 is syncing again (height: $FULL1_HEIGHT_RECONNECT)"; else echo "❌ full1 is NOT syncing"; exit 1; fi
✅ full1 is syncing again (height: ...)
```

## Finally, clean up

<details>
<summary>Stop the testnet and remove all data</summary>

```sh {timeout=120}
$ quake -v clean --all
...✅ Testnet is down...
...✅ Testnet data removed...
...✅ Monitoring data removed...
...✅ Testnet cleaned
```
</details>
