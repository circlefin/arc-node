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

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use color_eyre::eyre::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{env, fs};
use tracing::{info, warn};

use crate::infra::remote::CC_INSTANCE;
use crate::infra::terraform::TERRAFORM_STATE_FILENAME;
use crate::infra::{InfraData, INFRA_DATA_FILENAME};
use crate::testnet::{LAST_MANIFEST_FILENAME, QUAKE_DIR};

pub(crate) const SSH_KEY_FILENAME: &str = "ssh-private-key.pem";

/// JSON-serializable bundle for sharing a remote testnet.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportBundle {
    testnet_name: String,
    manifest_content: String,
    infra_data: serde_json::Value,
    ssh_private_key: String,
    controllers_config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terraform_state: Option<serde_json::Value>,
}

impl ExportBundle {
    fn validate(&self) -> Result<()> {
        if self.testnet_name.is_empty() {
            bail!("Export bundle has an empty testnet name");
        }
        if self.manifest_content.trim().is_empty() {
            bail!("Export bundle has an empty manifest");
        }
        if self.ssh_private_key.trim().is_empty() {
            bail!("Export bundle has an empty SSH private key");
        }
        if self.infra_data.is_null() {
            bail!("Export bundle has an empty infra-data");
        }
        if self.controllers_config.is_null() {
            bail!("Export bundle has no controllers config");
        }
        match &self.terraform_state {
            None => warn!(
                "Export bundle has no Terraform state — recipients cannot run terraform destroy"
            ),
            Some(state) => {
                let has_resources = state
                    .get("resources")
                    .and_then(|r| r.as_array())
                    .is_some_and(|r| !r.is_empty());
                if !has_resources {
                    warn!("Export bundle Terraform state has no resources — recipients cannot run terraform destroy");
                }
            }
        }
        Ok(())
    }
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path).wrap_err_with(|| format!("Failed to read {path:?}"))?;
    serde_json::from_str(&content).wrap_err_with(|| format!("Failed to parse {path:?}"))
}

fn read_optional_json_file(path: &Path) -> Option<serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn write_json_file(path: &Path, value: &serde_json::Value, label: &str) -> Result<()> {
    let json = serde_json::to_string_pretty(value)
        .wrap_err_with(|| format!("Failed to serialize {label}"))?;
    fs::write(path, json).wrap_err_with(|| format!("Failed to write {path:?}"))?;
    info!(path=%path.display(), "Wrote {label}");
    Ok(())
}

/// Export a JSON bundle containing everything needed for another user to
/// access this remote testnet via `quake remote import`.
pub(crate) fn export_testnet(
    testnet_dir: &Path,
    output_path: &Path,
    testnet_name: &str,
    manifest_path: &Path,
    exclude_terraform: bool,
) -> Result<()> {
    // Read all the files we need to export
    let manifest_content = fs::read_to_string(manifest_path)
        .wrap_err_with(|| format!("Failed to read manifest at {manifest_path:?}"))?;

    let infra_data = read_json_file(&testnet_dir.join(INFRA_DATA_FILENAME))?;

    let ssh_key_path = testnet_dir.join(SSH_KEY_FILENAME);
    let ssh_private_key = fs::read_to_string(&ssh_key_path)
        .wrap_err_with(|| format!("Failed to read {ssh_key_path:?}"))?;

    let controllers_config =
        read_json_file(&testnet_dir.join("assets").join("controllers-config.json"))?;

    let terraform_state = if exclude_terraform {
        None
    } else {
        let state = read_optional_json_file(&testnet_dir.join(TERRAFORM_STATE_FILENAME));
        if state.is_none() {
            warn!("No Terraform state found at {:?}: bundle will not include it. Use --exclude-terraform to suppress this warning.", testnet_dir.join(TERRAFORM_STATE_FILENAME));
        }
        state
    };

    // Serialize the bundle
    let bundle = ExportBundle {
        testnet_name: testnet_name.to_string(),
        manifest_content,
        infra_data,
        ssh_private_key,
        controllers_config,
        terraform_state,
    };

    let json =
        serde_json::to_string_pretty(&bundle).wrap_err("Failed to serialize export bundle")?;
    fs::write(output_path, json).wrap_err_with(|| format!("Failed to write {output_path:?}"))?;
    #[cfg(unix)]
    {
        fs::set_permissions(output_path, fs::Permissions::from_mode(0o600))
            .wrap_err_with(|| format!("Failed to set permissions on {output_path:?}"))?;
    }
    info!(path=%output_path.display(), "✅ Exported remote testnet bundle");
    Ok(())
}

/// Import a previously exported JSON bundle, writing the manifest,
/// infra-data.json, and SSH key so that quake commands work against the
/// remote testnet.
pub fn import_shared_testnet(path: &Path) -> Result<()> {
    // Read and validate the bundle
    let content = fs::read_to_string(path).wrap_err_with(|| format!("Failed to read {path:?}"))?;
    let bundle: ExportBundle = serde_json::from_str(&content)
        .wrap_err_with(|| format!("Failed to parse export bundle from {path:?}"))?;
    bundle.validate()?;

    // Read and validate the infra-data
    let infra_data: InfraData = serde_json::from_value(bundle.infra_data.clone())
        .wrap_err("Invalid infra-data in export bundle")?;
    infra_data
        .get_data(CC_INSTANCE)
        .wrap_err("Invalid infra-data in export bundle: missing control center details")?;

    // Create the testnet directory
    let repo_root = env::current_dir().wrap_err("Failed to get current working directory")?;
    let quake_dir = repo_root.join(QUAKE_DIR);
    let testnet_dir = quake_dir.join(bundle.testnet_name.replace('_', "-"));
    fs::create_dir_all(&testnet_dir)
        .wrap_err_with(|| format!("Failed to create {testnet_dir:?}"))?;

    // Write the manifest and infra-data
    let manifest_path = quake_dir.join(format!("{}.toml", bundle.testnet_name));
    fs::write(&manifest_path, &bundle.manifest_content)
        .wrap_err_with(|| format!("Failed to write manifest to {manifest_path:?}"))?;
    info!(path=%manifest_path.display(), "Wrote manifest");

    write_json_file(
        &testnet_dir.join(INFRA_DATA_FILENAME),
        &bundle.infra_data,
        "infra data",
    )?;

    // Write SSH private key with restricted permissions
    let ssh_key_path = testnet_dir.join(SSH_KEY_FILENAME);
    fs::write(&ssh_key_path, &bundle.ssh_private_key)
        .wrap_err_with(|| format!("Failed to write {ssh_key_path:?}"))?;
    #[cfg(unix)]
    {
        fs::set_permissions(&ssh_key_path, fs::Permissions::from_mode(0o600))
            .wrap_err_with(|| format!("Failed to set permissions on {ssh_key_path:?}"))?;
    }
    info!(path=%ssh_key_path.display(), "Wrote SSH private key");

    // Write the controllers config
    let assets_dir = testnet_dir.join("assets");
    fs::create_dir_all(&assets_dir).wrap_err_with(|| format!("Failed to create {assets_dir:?}"))?;
    write_json_file(
        &assets_dir.join("controllers-config.json"),
        &bundle.controllers_config,
        "controllers config",
    )?;

    // Write the Terraform state
    if let Some(terraform_state) = &bundle.terraform_state {
        write_json_file(
            &testnet_dir.join(TERRAFORM_STATE_FILENAME),
            terraform_state,
            "Terraform state",
        )?;
    }

    // Write .quake/.last_manifest so subsequent quake commands find the manifest
    let last_manifest_path = quake_dir.join(LAST_MANIFEST_FILENAME);
    fs::write(&last_manifest_path, manifest_path.display().to_string())
        .wrap_err_with(|| format!("Failed to write {last_manifest_path:?}"))?;

    info!(from=%path.display(), to=%manifest_path.display(), "✅ Imported remote testnet '{}'", bundle.testnet_name);
    Ok(())
}
