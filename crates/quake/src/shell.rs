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
use std::path::{Path, PathBuf};
use tracing::debug;

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
