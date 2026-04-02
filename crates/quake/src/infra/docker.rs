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

use color_eyre::eyre::{bail, Context, Result};
use std::path::Path;

use crate::shell;
use crate::testnet::DockerImages;

/// Check if the docker compose file exists
pub(crate) fn compose_file_exists(compose_path: &Path) -> Result<()> {
    if !compose_path.exists() {
        bail!("{} not found", compose_path.display());
    }
    Ok(())
}

pub(crate) fn exec(dir: &Path, args: Vec<&str>) -> Result<()> {
    shell::exec("docker", args, dir, None, false).wrap_err("Failed to execute docker command")
}

pub(crate) fn compose_exec(dir: &Path, compose_path: &Path, args: Vec<&str>) -> Result<()> {
    compose_file_exists(compose_path).wrap_err("Run `quake setup` to generate testnet files")?;

    let compose_path_str = compose_path
        .to_str()
        .expect("Failed to convert compose file path to string");
    let mut compose_args = vec!["compose", "-f", compose_path_str];
    compose_args.extend(args);
    shell::exec("docker", compose_args, dir, None, false)
        .wrap_err("Failed to execute docker compose command")
}

/// Run a one-off command in a service container via `docker compose run`.
///
/// Uses `--rm --no-deps` to avoid starting dependencies and to clean up after.
/// Stdout and stderr are inherited so output streams to the terminal.
pub(crate) fn compose_run(
    dir: &Path,
    compose_path: &Path,
    service: &str,
    entrypoint: &str,
    args: &[&str],
) -> Result<()> {
    compose_file_exists(compose_path).wrap_err("Run `quake setup` to generate testnet files")?;

    let compose_path_str = compose_path
        .to_str()
        .expect("Failed to convert compose file path to string");

    let mut cmd_args = vec![
        "compose",
        "-f",
        compose_path_str,
        "run",
        "--rm",
        "--no-deps",
        "--entrypoint",
        entrypoint,
        service,
    ];
    cmd_args.extend(args);

    let status = std::process::Command::new("docker")
        .args(&cmd_args)
        .current_dir(dir)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .wrap_err("Failed to execute docker compose run")?;

    if !status.success() {
        bail!("docker compose run exited with {status}");
    }

    Ok(())
}

/// Pull a Docker image from a remote registry.
pub(crate) fn pull(image: &str) -> Result<()> {
    shell::exec("docker", vec!["pull", image], Path::new("."), None, false)
        .wrap_err_with(|| format!("Failed to pull image {image}"))
}

/// Check if the given Docker images exist in the local Docker image store.
/// Returns an error if any image does not exist.
pub(crate) fn images_exist(images: &DockerImages) -> Result<()> {
    for tag in &images.all() {
        let filter = format!("reference={tag}");
        let args = vec![
            "images",
            "--format",
            "{{.Repository}}:{{.Tag}}",
            "--filter",
            &filter,
        ];
        let output = shell::exec_with_output("docker", args, Path::new("."))?;
        if output.trim().is_empty() {
            bail!("Docker image {tag} not found locally");
        }
    }
    Ok(())
}
