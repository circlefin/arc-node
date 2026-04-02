# Copyright 2026 Circle Internet Group, Inc. All rights reserved.
#
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""
md-exec.py - A tool for validating shell commands in Markdown.

Description:
    This tool parses Markdown files, extracts shell commands (prefixed with $ or >),
    executes them in a persistent shell environment, and validates the output against
    the expected output provided in the code block.

Usage:
    python3 md-exec.py [options] [path]

    [path]: Path to a single .md file or a directory containing .md files.
            If a directory is provided, it recursively processes all .md files.

Options:
    --abort-on-fail: Stop processing immediately after the first test failure.
    --print-command-output: Stream command output to the console in real-time.

Key Features:
    - **Persistence**: Environment variables and directory changes (cd) persist between
      commands within the same file.
    - **Ellipsis Matching**: Use `...` in expected output to match variable content
      (e.g., timestamps, IDs).
    - **Exit Codes**: Detects command failures. Use {exit_code...} to assert specific codes.
    - **Timeouts**: Specify timeouts for commands that might hang.

Code Block Attributes:
    Add these to the opening fence of your code block (e.g., ```sh {timeout=5})
    or inline within the command line (e.g., $ cmd {strip}).

    - {timeout=N}: Abort the command after N seconds.
    - {expected_fail}: Expect the test to fail (output mismatch or non-zero exit code).
    - {strip} or {ignore_trailing_new_line}: Remove trailing newlines/whitespace before comparing.
    - {empty_output}: Assert that the command produces no output (after stripping).
    - {exit_codeOPV}: Check exit code. OP is ==, !=, >, <, >=, <=. V is an integer.
                      Example: {exit_code==1}, {exit_code>0}.

Global Directives:
    Add these as HTML comments in your Markdown to affect all subsequent tests.
    Example: <!-- md-exec: abort_on_fail=true, print_command_output -->

    - abort_on_fail (true/false): Stop processing the file immediately after a failure.
    - print_command_output (true/false): Stream command output to the console in real-time.
"""

import sys
import re
import subprocess
import os
import argparse
import json
import tempfile
import textwrap
import time

class ShellTest:
    def __init__(self, command, expected_output, line_number, timeout=None, expected_fail=False, options=None, strip_output=False, exit_check=None, empty_output=False):
        self.command = command
        # Do not strip expected output; preserve newlines to distinguish "" from "\n"
        self.expected_output = expected_output
        self.line_number = line_number
        self.timeout = timeout
        self.expected_fail = expected_fail
        self.options = options or {}
        self.strip_output = strip_output
        self.exit_check = exit_check  # Tuple (operator, value) e.g. ("==", 1)
        self.empty_output = empty_output

class MarkdownParser:
    def __init__(self, filepath, cli_options=None):
        self.filepath = filepath
        self.cli_options = cli_options or {}

    def _create_test(self, cmd, expected_lines, line_num, timeout, expected_fail, options, strip_output, exit_check, empty_output):
        # Reconstruct expected output preserving shell-like behavior.
        # If there are expected lines, standard shell commands usually end with a newline.
        # ["foo"] -> "foo\n"
        # [""] -> "\n"
        # [] -> ""
        if expected_lines:
            expected_str = "\n".join(expected_lines) + "\n"
        else:
            expected_str = ""
        return ShellTest(cmd, expected_str, line_num, timeout, expected_fail, options.copy(), strip_output, exit_check, empty_output)

    def _extract_heredoc_delimiter(self, cmd):
        """Extract heredoc delimiter from a command if present.

        Supports patterns like:
        - << EOF
        - <<EOF
        - << 'EOF'
        - << "EOF"
        - <<- EOF (with tab stripping)

        Returns the delimiter string (unquoted) or None if no heredoc.
        """
        # Match heredoc pattern: <<[-][ ]?['"]?WORD['"]?
        match = re.search(r'<<-?\s*[\'"]?(\w+)[\'"]?\s*$', cmd)
        if match:
            return match.group(1)
        return None

    def parse(self):
        tests = []
        in_block = False
        current_cmd = None
        current_expected = []
        cmd_line_num = 0
        current_block_timeout = None
        current_cmd_expected_fail = False
        current_block_expected_fail = False
        current_block_strip = False
        current_cmd_strip = False
        current_block_exit_check = None
        current_cmd_exit_check = None
        current_block_empty_output = False
        current_cmd_empty_output = False
        current_block_indent = 0
        heredoc_delimiter = None  # Track if we're inside a heredoc

        # Configuration state that applies to tests based on file position
        # CLI options provide initial defaults, can be overridden by in-file directives
        current_options = {
            "abort_on_fail": self.cli_options.get("abort_on_fail", False),
            "print_command_output": self.cli_options.get("print_command_output", False)
        }

        # Queue of directives: (line_number, key, value)
        directive_queue = []

        with open(self.filepath, 'r') as f:
            content = f.read()

        # Pre-scan for md-exec directives before stripping comments
        directive_pattern = r'<!--\s*md-exec:\s*(.*?)\s*-->'
        for match in re.finditer(directive_pattern, content):
            line_num = content[:match.start()].count('\n')
            raw_text = match.group(1)

            # Split by comma to support multiple directives: "abort_on_fail, print_command_output"
            parts = [p.strip() for p in raw_text.split(',')]

            for part in parts:
                if '=' in part:
                    key, val = part.split('=', 1)
                    key = key.strip()
                    val = val.strip().lower()
                    parsed_val = True if val == 'true' else False if val == 'false' else val
                else:
                    key = part.strip()
                    parsed_val = True

                directive_queue.append((line_num, key, parsed_val))

        # Remove HTML comments but preserve line numbers
        content = re.sub(
            r'<!--.*?-->',
            lambda m: '\n' * m.group(0).count('\n'),
            content,
            flags=re.DOTALL
        )

        lines = content.splitlines(keepends=True)
        directive_idx = 0

        for i, line in enumerate(lines):
            # Update configuration if we passed a directive line
            while directive_idx < len(directive_queue) and directive_queue[directive_idx][0] <= i:
                d_line, d_key, d_val = directive_queue[directive_idx]
                current_options[d_key] = d_val
                directive_idx += 1

            stripped = line.strip()

            # Detect Code Snippets
            if stripped.startswith("```"):
                if in_block:
                    # End of block: save pending test
                    if current_cmd:
                        is_expected_fail = current_block_expected_fail or current_cmd_expected_fail
                        is_strip = current_block_strip or current_cmd_strip
                        is_empty = current_block_empty_output or current_cmd_empty_output
                        final_exit_check = current_cmd_exit_check if current_cmd_exit_check else current_block_exit_check
                        tests.append(self._create_test(
                            current_cmd,
                            current_expected,
                            cmd_line_num,
                            current_block_timeout,
                            is_expected_fail,
                            current_options,
                            is_strip,
                            final_exit_check,
                            is_empty
                        ))
                        current_cmd = None
                        current_expected = []
                        current_cmd_expected_fail = False
                        current_cmd_strip = False
                        current_cmd_exit_check = None
                        current_cmd_empty_output = False
                    in_block = False
                    current_block_timeout = None
                    current_block_expected_fail = False
                    current_block_strip = False
                    current_block_exit_check = None
                    current_block_empty_output = False
                    current_block_indent = 0
                else:
                    in_block = True
                    # Calculate indentation level of the code fence
                    current_block_indent = len(line) - len(line.lstrip())

                    # Parse attributes like {timeout=10}
                    timeout_match = re.search(r'timeout=(\d+)', stripped)
                    if timeout_match:
                        current_block_timeout = int(timeout_match.group(1))

                    # Parse {expected_fail}
                    if "{expected_fail}" in stripped:
                        current_block_expected_fail = True

                    # Parse {strip} or {ignore_trailing_new_line}
                    if "{strip}" in stripped or "{ignore_trailing_new_line}" in stripped:
                        current_block_strip = True

                    # Parse {empty_output}
                    if "{empty_output}" in stripped:
                        current_block_empty_output = True

                    # Parse {exit_code...}
                    # Matches exit_code followed by operator and number e.g. exit_code==1, exit_code>5
                    exit_match = re.search(r'exit_code(==|!=|>=|<=|>|<)(\d+)', stripped)
                    if exit_match:
                        current_block_exit_check = (exit_match.group(1), int(exit_match.group(2)))
                continue

            if in_block:
                # Check for Command Prompts ($ or >)
                if line.lstrip().startswith("$ ") or line.lstrip().startswith("> "):
                    # Save previous command if exists
                    if current_cmd:
                        is_expected_fail = current_block_expected_fail or current_cmd_expected_fail
                        is_strip = current_block_strip or current_cmd_strip
                        is_empty = current_block_empty_output or current_cmd_empty_output
                        final_exit_check = current_cmd_exit_check if current_cmd_exit_check else current_block_exit_check
                        tests.append(self._create_test(
                            current_cmd,
                            current_expected,
                            cmd_line_num,
                            current_block_timeout,
                            is_expected_fail,
                            current_options,
                            is_strip,
                            final_exit_check,
                            is_empty
                        ))
                        current_expected = []
                        current_cmd_expected_fail = False
                        current_cmd_strip = False
                        current_cmd_exit_check = None
                        current_cmd_empty_output = False

                    # Extract new command
                    prompt_index = line.find("$ ")
                    if prompt_index == -1: prompt_index = line.find("> ")

                    raw_cmd = line[prompt_index + 2:].strip()

                    # Inline attribute parsing

                    # {expected_fail}
                    if "{expected_fail}" in raw_cmd:
                        current_cmd_expected_fail = True
                        raw_cmd = raw_cmd.replace("{expected_fail}", "").strip()
                    else:
                        current_cmd_expected_fail = False

                    # {strip} or {ignore_trailing_new_line}
                    if "{strip}" in raw_cmd:
                        current_cmd_strip = True
                        raw_cmd = raw_cmd.replace("{strip}", "").strip()
                    elif "{ignore_trailing_new_line}" in raw_cmd:
                        current_cmd_strip = True
                        raw_cmd = raw_cmd.replace("{ignore_trailing_new_line}", "").strip()
                    else:
                        current_cmd_strip = False

                    # {empty_output}
                    if "{empty_output}" in raw_cmd:
                        current_cmd_empty_output = True
                        raw_cmd = raw_cmd.replace("{empty_output}", "").strip()
                    else:
                        current_cmd_empty_output = False

                    # {exit_code...} inline
                    inline_exit_match = re.search(r'\{exit_code(==|!=|>=|<=|>|<)(\d+)\}', raw_cmd)
                    if inline_exit_match:
                        current_cmd_exit_check = (inline_exit_match.group(1), int(inline_exit_match.group(2)))
                        raw_cmd = raw_cmd.replace(inline_exit_match.group(0), "").strip()
                    else:
                        current_cmd_exit_check = None

                    current_cmd = raw_cmd
                    cmd_line_num = i + 1

                    # Check if this command starts a heredoc
                    heredoc_delimiter = self._extract_heredoc_delimiter(raw_cmd)

                elif current_cmd is not None:
                    raw_line = line.rstrip('\r\n')
                    # Remove block indentation if present
                    if current_block_indent > 0 and len(raw_line) >= current_block_indent:
                        raw_line = raw_line[current_block_indent:]

                    # If we're in a heredoc, append to command
                    if heredoc_delimiter is not None:
                        current_cmd += "\n" + raw_line
                        # Check if this line ends the heredoc
                        if raw_line.strip() == heredoc_delimiter:
                            heredoc_delimiter = None
                    else:
                        # This is expected output
                        current_expected.append(raw_line)

        # Catch trailing command at end of file
        if current_cmd:
             is_expected_fail = current_block_expected_fail or current_cmd_expected_fail
             is_strip = current_block_strip or current_cmd_strip
             is_empty = current_block_empty_output or current_cmd_empty_output
             final_exit_check = current_cmd_exit_check if current_cmd_exit_check else current_block_exit_check
             tests.append(self._create_test(
                 current_cmd,
                 current_expected,
                 cmd_line_num,
                 current_block_timeout,
                 is_expected_fail,
                 current_options,
                 is_strip,
                 final_exit_check,
                 is_empty
             ))

        return tests

class TestRunner:
    def __init__(self):
        self.env = os.environ.copy()
        self.cwd = os.getcwd()
        self.oldpwd = self.cwd  # Track OLDPWD for 'cd -' support
        self.dirstack = []  # Track directory stack for pushd/popd
        self.last_exit_code = 0

    def run(self, tests, file_path):
        passed = 0
        failed = 0

        print(f"Running {len(tests)} code snippets...\n")

        for test in tests:
            print(f"▶️ {file_path}:{test.line_number}: {test.command} ... ", end='', flush=True)

            with tempfile.NamedTemporaryFile(mode='w+', delete=False) as env_dump:
                env_dump_path = env_dump.name

            state_dumper = (
                f"{sys.executable} -c "
                f"'import os, json; "
                f"d=dict(os.environ); "
                f"d[\"__CWD__\"]=os.getcwd(); "
                f"d[\"__EXIT__\"]=os.getenv(\"__RET\", \"0\"); "
                f"print(json.dumps(d))' > {env_dump_path}"
            )

            # Set OLDPWD for 'cd -' support
            self.env["OLDPWD"] = self.oldpwd

            # Handle pushd/popd specially since each command runs in a fresh shell
            command = test.command
            command_stripped = command.strip()

            if command_stripped.startswith("pushd "):
                # pushd: save current dir to stack, then cd
                target_dir = command_stripped[6:].strip()
                self.dirstack.append(self.cwd)
                # Let the shell do the pushd, but we track the stack ourselves
            elif command_stripped == "popd":
                if self.dirstack:
                    # Replace popd with cd to the popped directory
                    popped = self.dirstack.pop()
                    command = f"cd {popped}"

            # Export OLDPWD for 'cd -' support (must be shell variable, not just env)
            full_command = (
                f"export OLDPWD='{self.oldpwd}'\n"
                f"(exit {self.last_exit_code})\n"
                f"{command}\n"
                f"export __RET=$?\n"
                f"{state_dumper}"
            )

            stream_output = test.options.get("print_command_output", False)
            if stream_output:
                print() # Start output on new line if streaming

            start_time = time.time()
            try:
                # Use Popen to allow streaming, use bash for better OLDPWD/pushd/popd support
                with subprocess.Popen(
                    full_command,
                    shell=True,
                    executable='/bin/bash',
                    cwd=self.cwd,
                    env=self.env,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    bufsize=1
                ) as proc:
                    actual_output_chunks = []

                    if test.timeout:
                        # Use communicate with timeout for reliable timeout handling
                        try:
                            actual_output, _ = proc.communicate(timeout=test.timeout)
                            if stream_output and actual_output:
                                print(actual_output, end='', flush=True)
                        except subprocess.TimeoutExpired:
                            proc.kill()
                            proc.wait()
                            raise subprocess.TimeoutExpired(test.command, test.timeout, output="")
                    else:
                        # No timeout - stream line by line
                        for line in proc.stdout:
                            actual_output_chunks.append(line)
                            if stream_output:
                                print(line, end='', flush=True)
                        proc.wait()
                        actual_output = "".join(actual_output_chunks)

                if os.path.exists(env_dump_path) and os.path.getsize(env_dump_path) > 0:
                    with open(env_dump_path, 'r') as f:
                        try:
                            new_state = json.load(f)
                            if "__CWD__" in new_state:
                                new_cwd = new_state.pop("__CWD__")
                                # Update OLDPWD if directory changed
                                if new_cwd != self.cwd:
                                    self.oldpwd = self.cwd
                                self.cwd = new_cwd
                            if "__EXIT__" in new_state:
                                self.last_exit_code = int(new_state.pop("__EXIT__"))
                            if "__RET" in new_state:
                                del new_state["__RET"]
                            self.env = new_state
                        except (json.JSONDecodeError, ValueError):
                            pass

            except subprocess.TimeoutExpired as e:
                actual_output = e.stdout or ""
                self.last_exit_code = 1
                if stream_output:
                    print(f"\n[Timeout after {test.timeout}s]")

            except Exception as e:
                actual_output = f"Error executing command: {e}"
            finally:
                if os.path.exists(env_dump_path):
                    os.remove(env_dump_path)

            # Apply strip option if enabled
            expected_val = test.expected_output
            actual_val = actual_output

            if test.strip_output:
                expected_val = expected_val.rstrip('\r\n')
                actual_val = actual_val.rstrip('\r\n')

            match = False
            if test.empty_output:
                # If empty_output is requested, valid if trimmed actual output is empty
                match = (actual_output.strip() == "")
            else:
                match = self.check_match(expected_val, actual_val)

            # Check exit code if specified
            exit_code_ok = True
            if test.exit_check:
                op, target = test.exit_check
                actual_exit = self.last_exit_code
                if op == '==': exit_code_ok = (actual_exit == target)
                elif op == '!=': exit_code_ok = (actual_exit != target)
                elif op == '>': exit_code_ok = (actual_exit > target)
                elif op == '<': exit_code_ok = (actual_exit < target)
                elif op == '>=': exit_code_ok = (actual_exit >= target)
                elif op == '<=': exit_code_ok = (actual_exit <= target)

            # Overall success requires both output match and exit code check
            actual_success = match and exit_code_ok
            test_passed = False

            if test.expected_fail:
                if not actual_success:
                    print("✅ PASS (Expected Failure)")
                    passed += 1
                    test_passed = True
                else:
                    print(f"❌ FAIL (Unexpected Success -- {file_path}:{test.line_number})")
                    if not match: print("  Output matched unexpectedly.")
                    if not exit_code_ok: print("  Exit code satisfied unexpectedly.")
                    failed += 1
            else:
                if actual_success:
                    print("✅ PASS")
                    passed += 1
                    test_passed = True
                else:
                    print(f"❌ FAIL ({file_path}:{test.line_number})")
                    if not match:
                        print("  Expected Output:")
                        if test.empty_output:
                             print("    (empty)")
                        elif expected_val:
                            print(textwrap.indent(expected_val, "    ", predicate=lambda _: True), end='' if expected_val.endswith('\n') else '\n')
                        else:
                            print("    (empty)")

                        if not stream_output:
                            print("  Actual Output:")
                            if actual_val:
                                print(textwrap.indent(actual_val, "    ", predicate=lambda _: True), end='' if actual_val.endswith('\n') else '\n')
                            else:
                                print("    (empty)")

                    if not exit_code_ok:
                        op, target = test.exit_check
                        print(f"  Expected Exit Code: {op} {target}")
                        print(f"  Actual Exit Code:   {self.last_exit_code}")
                    failed += 1

            # Print a newline if streaming output to avoid clobbering the next test
            if stream_output:
                print()

            if not test_passed and test.options.get("abort_on_fail"):
                print("🛑 Aborting tests due to failure (abort_on_fail is active).")
                print(f"\nResults: {passed} passed, {failed} failed (aborted).")
                return False

        print(f"\nResults: {passed} passed, {failed} failed.")
        return failed == 0

    def check_match(self, expected, actual):
        if expected == actual:
            return True

        escaped = re.escape(expected)
        # Replace escaped ellipsis (\.\.\.) with regex wildcard (.*)
        # We use .* to match across lines (DOTALL)
        # Newlines before and after ellipsis are discarded (optional)
        # Note: re.escape escapes \n as backslash + newline, so we match that
        escaped_newline = '\\' + '\n'
        pattern = escaped.replace(escaped_newline + '\\.\\.\\.' + escaped_newline, '.*')
        pattern = pattern.replace('\\.\\.\\.' + escaped_newline, '.*')
        pattern = pattern.replace(escaped_newline + '\\.\\.\\.', '.*')
        pattern = pattern.replace('\\.\\.\\.', '.*')
        regex = f"^{pattern}$"

        return bool(re.match(regex, actual, re.DOTALL))

if __name__ == "__main__":
    arg_parser = argparse.ArgumentParser(description="Execute shell commands found in code snippets of markdown files and validate their output.")
    arg_parser.add_argument("path", help="Markdown file or directory to validate")
    arg_parser.add_argument("--abort-on-fail", action="store_true", help="Stop processing immediately after the first test failure")
    arg_parser.add_argument("--print-command-output", action="store_true", help="Stream command output to the console in real-time")
    args = arg_parser.parse_args()

    cli_options = {
        "abort_on_fail": args.abort_on_fail,
        "print_command_output": args.print_command_output
    }

    if not os.path.exists(args.path):
        print(f"Error: Path '{args.path}' not found.")
        sys.exit(1)

    is_dir_mode = os.path.isdir(args.path)
    files_to_process = []

    if is_dir_mode:
        for root, _, files in os.walk(args.path):
            for file in files:
                if file.lower().endswith(".md"):
                    files_to_process.append(os.path.join(root, file))
    else:
        files_to_process.append(args.path)

    if not files_to_process:
        print(f"No markdown files found in {args.path}")
        sys.exit(0)

    overall_success = True
    results_summary = []

    for filepath in files_to_process:
        print(f"Checking {filepath}...")
        md_parser = MarkdownParser(filepath, cli_options)
        tests = md_parser.parse()

        status = "SKIP"
        if not tests:
            print("No shell tests found in document.\n")
        else:
            runner = TestRunner()
            if runner.run(tests, filepath):
                status = "PASS"
            else:
                status = "FAIL"
                overall_success = False

        results_summary.append((filepath, status))
        print() # Newline between files

    if is_dir_mode:
        print("=" * 60)
        print(f"SUMMARY for directory: {args.path}")
        print("=" * 60)
        for fpath, status in results_summary:
            rel_path = os.path.relpath(fpath, args.path)
            icon = "✅" if status == "PASS" else "❌" if status == "FAIL" else "⚠️"
            print(f"{icon} {status.ljust(5)} : {rel_path}")
        print("=" * 60)

    sys.exit(0 if overall_success else 1)
