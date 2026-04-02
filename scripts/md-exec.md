# md-exec

**Executable documentation for shell commands in Markdown.**

`md-exec` keeps your documentation in sync with actual behavior.
It parses Markdown files, extracts shell commands from code blocks, executes them, and validates the output against expected results.

## Usage

```
python3 md-exec.py [options] <path>
```

**Arguments:**
- `<path>` — A Markdown file or directory. If a directory is provided, all `.md` files are processed recursively.

**Options:**
- `--abort-on-fail` — Stop immediately after the first test failure.
- `--print-command-output` — Stream command output to the console in real-time.

## Key Features

### Ellipsis pattern matching

Use `...` in expected output to match any text. This lets you ignore variable content like timestamps, IDs, or paths that change between runs.

### Merged output streams

Both `stdout` and `stderr` are captured together. This is essential for tools like Docker that print status messages to `stderr`.

### Persistent state

- **Environment variables** persist across code blocks within the same file.
- **Working directory** changes via `cd`, `pushd`, and `popd` are tracked and preserved.
- **Docker** works natively since container state is managed by the Docker daemon.

---

## Examples

This documentation is itself a test suite for `md-exec`. Validate it with `python3 md-exec.py md-exec.md`.

### Basic usage

A code snippet contains a shell command (prefixed with `$`) and its expected output:
```sh
$ echo Hola mundo
Hola mundo
```

Multiple commands and outputs can be placed in the same snippet:
```sh
$ echo foo
foo
$ echo bar
bar
```

### Environment variables

Define an environment variable in one snippet...
```sh
$ export FOO=123
```

...and use it in a later snippet:
```sh
$ echo $FOO
123
```

### Ellipsis matching

Use `...` to match any substring in the output:
```sh
$ echo The Width of a Circle
...Width...Circle...
```

It works across newlines too:
```sh
$ printf "*** foo\n *** bar\n *** baz\n ***\n"
...foo...bar...baz...
```

You can also use multiple matchers on separate lines:
```sh
$ printf "*** foo\n *** bar\n *** baz\n ***\n"
...foo...
...bar...
...baz...
```

New lines before and after `...` are discarded:
```sh
$ printf "foo"
...
foo
...
```

### Commented code snippets are ignored

Code blocks inside HTML comments are skipped.

<!--
```sh
$ echo 1
2
```
-->

### Changing directories

Directory changes persist between commands. Both `cd -` (return to previous directory) and `pushd`/`popd` (directory stack) are supported:
```sh
$ cd /tmp
$ pwd
/tmp
$ cd -
...
```

```sh
$ pushd /tmp
/tmp ...
$ pwd
/tmp
$ popd
```

### Indented code snippets

Code blocks inside HTML elements (like `<details>`) are detected and executed correctly:
<details>
<summary>Collapsable</summary>

    ```sh
    $ echo spam spam spam
    spam spam spam
    ```
</details>

---

## Code Block Options

Add options to the opening fence (e.g., `` ```sh {timeout=5} ``) or inline with the command.

### Expected failures

Mark a test that is expected to fail with `{expected_fail}`:
```sh {expected_fail}
$ true 
1
```

### Exit code assertions

Check exit codes using `echo $?`:
```sh
$ true
$ echo $?
0
```

```sh
$ false
$ echo $?
1
```

Or use the `{exit_code}` option for a more concise syntax. Supported operators: `==`, `!=`, `>`, `<`, `>=`, `<=`.
```sh {exit_code==1}
$ false
```

```sh {exit_code>0}
$ false
```

### Empty output

Assert that a command produces no output (ignoring whitespace) with `{empty_output}`:
```sh {empty_output}
$ printf ""
```

```sh {empty_output}
$ printf "\n \n"
```

### Strip trailing whitespace

Use `{strip}` when the command output differs only in trailing newlines:
```sh {strip}
$ printf "\nfoo"

foo
```
You can also use `{ignore_trailing_new_line}` as an alias for `{strip}`.

### Timeout

Set a maximum execution time with `{timeout=N}`. If exceeded, the command is killed and the exit code is set to 1:
```sh {timeout=1}
$ sleep 5
$ echo $?
1
```

### Multi-line commands (heredocs)

Heredoc syntax is supported for multi-line input:
```sh {empty_output}
$ cat > /tmp/md-exec.txt << 'EOF'
bla
blah
EOF
```

---

## Global Directives

Add directives as HTML comments to affect all subsequent tests in a file:

```html
<!-- md-exec: abort_on_fail=true, print_command_output -->
```

- `abort_on_fail` — Stop processing after the first failure.
- `print_command_output` — Print command output to the console as it runs.

These can also be set via CLI flags (`--abort-on-fail`, `--print-command-output`), which apply globally. In-file directives override CLI flags for tests that follow them.
