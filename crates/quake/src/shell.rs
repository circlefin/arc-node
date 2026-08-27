// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use color_eyre::eyre::{bail, eyre, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// Execute a command in a given directory
///
/// Arguments:
/// - cmd: the command to execute
/// - args: the arguments to pass to the command
/// - dir: the directory to execute the command in
/// - envs: the environment variables to set for the command
/// - background: if true, the command will run in the background and won't block
pub(crate) fn exec(
    cmd: &str,
    args: Vec<&str>,
    dir: &Path,
    envs: Option<Vec<(String, String)>>,
    background: bool,
) -> Result<()> {
    debug!(%cmd, args=%args.join(" "), dir=%dir.display(), "Executing");

    let mut child = std::process::Command::new(cmd);
    let mut child = child
        .args(&args)
        .current_dir(dir)
        .stdout(if background {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        })
        .stderr(if background {
            std::process::Stdio::null()
        } else {
            std::process::Stdio::inherit()
        });

    if let Some(envs) = envs {
        for (key, value) in envs {
            child = child.env(key, value);
        }
    }

    let mut child = child
        .spawn()
        .wrap_err_with(|| format!("Failed to execute {cmd} {}", args.join(" ")))?;

    if background {
        Ok(())
    } else {
        let status = child
            .wait()
            .wrap_err_with(|| format!("Failed to wait for {cmd} {}", args.join(" ")))?;
        if status.success() {
            Ok(())
        } else {
            let code = status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            bail!(format!(
                "Command failed with exit code {code}: {cmd} {}",
                args.join(" ")
            ))
        }
    }
}

/// Execute a command in a given directory and return the output
pub(crate) fn exec_with_output(cmd: &str, args: Vec<&str>, dir: &Path) -> Result<String> {
    debug!(%cmd, args=%args.join(" "), dir=%dir.display(), "Executing");

    let output = std::process::Command::new(cmd)
        .args(&args)
        .current_dir(dir)
        .output()
        .wrap_err_with(|| format!("Failed to execute {cmd} {}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(format!("Command failed: {stderr}"))
    }
}

/// Like `exec_with_output`, but streams each stdout/stderr line through the
/// runner's tracing infrastructure as it arrives, while still capturing the
/// full stdout for return. Use this when the wrapped subprocess is long-lived
/// (e.g. a remote spammer phase) and the user needs live visibility into what
/// it's doing rather than waiting for the buffered output at the end.
///
/// `prefix` is prepended to each forwarded line so it's distinguishable from
/// the runner's own logs (e.g. `prefix="spammer"` produces `spammer | …`).
pub(crate) fn exec_with_streaming_output(
    cmd: &str,
    args: Vec<&str>,
    dir: &Path,
    prefix: &str,
) -> Result<String> {
    debug!(%cmd, args=%args.join(" "), dir=%dir.display(), "Executing (streaming)");

    let mut child = std::process::Command::new(cmd)
        .args(&args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err_with(|| format!("Failed to spawn {cmd} {}", args.join(" ")))?;

    let stdout = child.stdout.take().expect("stdout piped by spawn config");
    let stderr = child.stderr.take().expect("stderr piped by spawn config");

    let captured_stdout = Arc::new(Mutex::new(String::new()));
    let captured_stderr = Arc::new(Mutex::new(String::new()));

    let stdout_buf = Arc::clone(&captured_stdout);
    let stdout_prefix = prefix.to_string();
    let stdout_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            info!("{stdout_prefix} | {line}");
            if let Ok(mut buf) = stdout_buf.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    });

    let stderr_buf = Arc::clone(&captured_stderr);
    let stderr_prefix = prefix.to_string();
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            warn!("{stderr_prefix} | {line}");
            if let Ok(mut buf) = stderr_buf.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    });

    let status = child
        .wait()
        .wrap_err_with(|| format!("Failed to wait for {cmd} {}", args.join(" ")))?;
    stdout_thread.join().expect("stdout reader thread panicked");
    stderr_thread.join().expect("stderr reader thread panicked");

    let stdout_str = captured_stdout.lock().expect("stdout mutex").clone();
    let stderr_str = captured_stderr.lock().expect("stderr mutex").clone();

    if status.success() {
        Ok(stdout_str.trim().to_string())
    } else {
        bail!("Command failed: {}", stderr_str.trim());
    }
}

/// Return the relative path to the given base path
pub(crate) fn relative_path(path: &PathBuf, base: &PathBuf) -> Result<PathBuf> {
    pathdiff::diff_paths(path, base)
        .ok_or_else(|| eyre!("Failed to calculate relative path from {base:?} to {path:?}"))
}

/// Generate SSH options with a reason for the SSM session.
///
/// ProxyCommand is used to start a short-lived SSM session to the remote host.
/// The reason parameter identifies the session in AWS SSM, making it easier to
/// track which sessions were created by quake SSH/SCP commands.
fn ssh_opts(host: &str) -> [String; 6] {
    let reason = format!("quake-ssh-{}", host);
    [
        "-o".to_string(), "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(), "LogLevel=ERROR".to_string(),
        // Quoted to avoid issues with the shell
        "-o".to_string(), format!("\"ProxyCommand=aws ssm start-session --target %h --document-name AWS-StartSSHSession --parameters portNumber=%p --reason {reason}\""),
    ]
}

/// SSH to a remote host. If a command is not provided, an interactive shell will be opened.
///
/// If `force_tty` is true, a PTY is allocated even when a command is provided
/// (useful for nested interactive SSH sessions).
pub(crate) fn ssh(
    host: &str,
    user_name: &str,
    private_key_path: &str,
    dir: &Path,
    cmd: &str,
    force_tty: bool,
) -> Result<()> {
    let ssh_opts = ssh_opts(host).join(" ");
    let tty_flag = if cmd.is_empty() || force_tty {
        // Interactive session: force PTY allocation with -t
        "-tt "
    } else {
        ""
    };
    let args = if cmd.is_empty() {
        format!("{ssh_opts} -i {private_key_path} {tty_flag}{user_name}@{host}")
    } else {
        format!("{ssh_opts} -i {private_key_path} {tty_flag}{user_name}@{host} \"{cmd}\"")
    };
    let cmd = format!("ssh {args}");
    exec("bash", vec!["-c", cmd.as_str()], dir, None, false)
}

/// SSH to a remote host and return the command's stdout.
///
/// Unlike `ssh`, this variant captures output instead of inheriting stdio,
/// making it suitable for programmatic use (e.g. MCP tools).
pub(crate) fn ssh_with_output(
    host: &str,
    user_name: &str,
    private_key_path: &str,
    dir: &Path,
    cmd: &str,
) -> Result<String> {
    let ssh_opts = ssh_opts(host).join(" ");
    let args = format!("{ssh_opts} -i {private_key_path} {user_name}@{host} \"{cmd}\"");
    let full_cmd = format!("ssh {args}");
    exec_with_output("bash", vec!["-c", full_cmd.as_str()], dir)
}

/// Same as [`ssh_with_output`], but streams the remote command's stdout/stderr
/// line-by-line through the runner's tracing infrastructure as it arrives. Use
/// this for long-lived remote commands (e.g. a saturation phase's spammer
/// subprocess) where buffering the output until completion would hide what the
/// remote process is currently doing.
pub(crate) fn ssh_with_streaming_output(
    host: &str,
    user_name: &str,
    private_key_path: &str,
    dir: &Path,
    cmd: &str,
    prefix: &str,
) -> Result<String> {
    let ssh_opts = ssh_opts(host).join(" ");
    let args = format!("{ssh_opts} -i {private_key_path} {user_name}@{host} \"{cmd}\"");
    let full_cmd = format!("ssh {args}");
    exec_with_streaming_output("bash", vec!["-c", full_cmd.as_str()], dir, prefix)
}

/// Copy multiple files or directories to a remote server.
///
/// Arguments:
/// - host: IP address or EC2 instance ID of the remote server.
/// - user_name: User name to use for connecting to the remote server.
/// - private_key_path: Path to the private key to use when connecting to the remote server.
/// - dir: Local directory to execute the scp command in.
/// - sources: List of local files or directories to copy.
/// - dest: Path to the destination directory on the user's home directory on the remote server.
///   If not provided, the files will be copied to the remote server's home directory.
/// - recursive: If true, the source files or directories will be copied recursively.
pub(crate) fn scp(
    host: &str,
    user_name: &str,
    private_key_path: &str,
    dir: &Path,
    sources: &[&str],
    dest: &str,
    recursive: bool,
) -> Result<()> {
    let opts = ssh_opts(host);
    // Trim any quotes to avoid issues with the shell
    let mut args: Vec<&str> = opts.iter().map(|s| s.trim_matches('"')).collect::<Vec<_>>();

    args.extend(vec!["-i", private_key_path]);

    args.push("-C"); // compress

    if recursive {
        args.push("-r");
    }
    args.extend(sources);

    let dest = format!("{user_name}@{host}:/home/{user_name}/{dest}");
    args.push(&dest);

    exec("scp", args, dir, None, false).wrap_err_with(|| format!("Failed to copy files to {host}"))
}

/// Copy a file from a remote server to a local path.
///
/// `remote_source` is relative to `/home/{user_name}/` on the remote server.
pub(crate) fn scp_from(
    host: &str,
    user_name: &str,
    private_key_path: &str,
    dir: &Path,
    remote_source: &str,
    local_dest: &Path,
) -> Result<()> {
    let opts = ssh_opts(host);
    let mut args: Vec<&str> = opts.iter().map(|s| s.trim_matches('"')).collect::<Vec<_>>();
    args.extend(vec!["-i", private_key_path]);
    args.push("-C"); // compress
    let source = format!("{user_name}@{host}:/home/{user_name}/{remote_source}");
    let dest = local_dest.to_string_lossy().into_owned();
    args.push(&source);
    args.push(&dest);
    exec("scp", args, dir, None, false)
        .wrap_err_with(|| format!("Failed to copy files from {host}"))
}
