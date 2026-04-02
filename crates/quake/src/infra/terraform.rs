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
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use std::{env, fs};
use tracing::info;

use crate::{
    infra::BLOCKSCOUT_SSM_PORT,
    node::{NodeName, SubnetName},
    shell, testnet,
};

pub(crate) const TERRAFORM_STATE_FILENAME: &str = "terraform.tfstate";

/// Manage Terraform state and remote infrastructure
pub(crate) struct Terraform {
    /// Directory containing the Terraform files
    terraform_dir: PathBuf,
    /// Directory containing the testnet files
    testnet_dir: PathBuf,
    /// Path to the manifest file
    pub(crate) manifest_file: PathBuf,
    /// Per-testnet state file so multiple testnets don't share state
    state_file: PathBuf,
    /// Resolved images for the remote containers
    images: testnet::DockerImages,
    /// List of node names
    node_names: Vec<String>,
    /// Network topology as a map of subnet name to the list of node names in that subnet
    network_topology: IndexMap<SubnetName, Vec<NodeName>>,
}

impl Terraform {
    pub(crate) fn new(
        terraform_dir: &Path,
        testnet_dir: &Path,
        manifest_file: &Path,
        images: testnet::DockerImages,
        node_names: Vec<String>,
        network_topology: IndexMap<SubnetName, Vec<NodeName>>,
    ) -> Result<Self> {
        if node_names.is_empty() {
            bail!("No node names provided for managing remote infra");
        }

        // state_file must be absolute because terraform commands run with
        // cwd = terraform_dir, so a relative -state path would resolve there.
        let cwd = env::current_dir().wrap_err("Failed to get current working directory")?;
        let state_file = cwd.join(testnet_dir).join(TERRAFORM_STATE_FILENAME);

        Ok(Self {
            terraform_dir: terraform_dir.to_path_buf(),
            testnet_dir: testnet_dir.to_path_buf(),
            manifest_file: manifest_file.to_path_buf(),
            state_file,
            images,
            node_names,
            network_topology,
        })
    }

    /// Initialize Terraform plugins and state (needed only once)
    pub(crate) fn init(&self) -> Result<()> {
        shell::exec("terraform", vec!["init"], &self.terraform_dir, None, false)
    }

    /// Create the nodes and the Control Center server in the remote infrastructure.
    ///
    /// `node_size` and `cc_size` override the Terraform defaults for EC2 instance types.
    pub(crate) fn create(
        &self,
        dry_run: bool,
        yes: bool,
        node_size: Option<&str>,
        cc_size: Option<&str>,
    ) -> Result<()> {
        // Ensure testnet directory exists
        if !dry_run {
            std::fs::create_dir_all(&self.testnet_dir)
                .wrap_err("Failed to create testnet directory")?;
        }

        let mut args: Vec<&str> = vec![if dry_run { "plan" } else { "apply" }];

        let vars = self.build_variables(&self.node_names, node_size, cc_size)?;
        args.extend(vars.iter().map(String::as_str));

        let state_flag = self.state_flag();
        args.push(&state_flag);

        if yes {
            args.push("--auto-approve");
        }

        shell::exec("terraform", args, &self.terraform_dir, None, false)?;
        info!(dir=%self.terraform_dir.display(), "✅ Remote infrastructure created via Terraform");
        Ok(())
    }

    /// Destroy the created infrastructure
    pub(crate) fn destroy(&self, yes: bool) -> Result<()> {
        let mut args: Vec<&str> = vec!["destroy"];

        let vars = self.build_variables(&self.node_names, None, None)?;
        args.extend(vars.iter().map(String::as_str));

        let state_flag = self.state_flag();
        args.push(&state_flag);

        if yes {
            args.push("--auto-approve");
        }

        shell::exec("terraform", args, &self.terraform_dir, None, false)?;
        info!(dir=%self.terraform_dir.display(), "✅ Remote infrastructure destroyed via Terraform");
        Ok(())
    }

    /// Whether Terraform state with tracked resources exists for this testnet.
    /// Returns false if the file is missing, unreadable, or has an empty resources array.
    pub(crate) fn has_state(&self) -> bool {
        let Ok(content) = fs::read_to_string(&self.state_file) else {
            return false;
        };
        let state: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => return false,
        };
        state
            .get("resources")
            .and_then(|r| r.as_array())
            .is_some_and(|r| !r.is_empty())
    }

    fn state_flag(&self) -> String {
        format!("-state={}", self.state_file.display())
    }

    // Build variables for passing as arguments to Terraform commands
    fn build_variables(
        &self,
        node_names: &[String],
        node_size: Option<&str>,
        cc_size: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut args: Vec<String> = Vec::new();

        // Add variables
        args.push("-var".to_string());
        args.push(format!("testnet_dir={}", self.testnet_dir.display()));

        args.push("-var".to_string());
        args.push(format!("manifest_path={}", self.manifest_file.display()));

        args.push("-var".to_string());
        args.push(format!("image_cl={}", self.images.cl));
        args.push("-var".to_string());
        args.push(format!("image_el={}", self.images.el));

        args.push("-var".to_string());
        args.push(format!("blockscout_ssm_port={}", BLOCKSCOUT_SSM_PORT));

        // Add node names list as HCL expression (no shell quoting needed)
        let node_names_expr = format!(
            "node_names=[{}]",
            node_names
                .iter()
                .map(|n| format!("\"{}\"", n))
                .collect::<Vec<_>>()
                .join(",")
        );
        args.push("-var".to_string());
        args.push(node_names_expr);

        // Add network topology as HCL map expression
        // Format: network_topology={subnet1=["node1","node2"],subnet2=["node3"]}
        let network_topology_expr = format!(
            "network_topology={{{}}}",
            self.network_topology
                .iter()
                .map(|(subnet, nodes)| {
                    let nodes_list = nodes
                        .iter()
                        .map(|n| format!("\"{}\"", n))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("{}=[{}]", subnet, nodes_list)
                })
                .collect::<Vec<_>>()
                .join(",")
        );
        args.push("-var".to_string());
        args.push(network_topology_expr);

        let github_user = dotenvy::var("GITHUB_USER")
            .wrap_err("GITHUB_USER not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("github_user={}", github_user));

        let github_token = dotenvy::var("GITHUB_TOKEN")
            .wrap_err("GITHUB_TOKEN not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("github_token={}", github_token));

        let circle_base_image = dotenvy::var("CIRCLE_BASE_IMAGE")
            .wrap_err("CIRCLE_BASE_IMAGE not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("circle_base_image={}", circle_base_image));

        let ami_owner = dotenvy::var("EC2_AMI_OWNER")
            .wrap_err("EC2_AMI_OWNER not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("ami_owner={}", ami_owner));

        let ami_name_filter = dotenvy::var("EC2_AMI_NAME_FILTER")
            .wrap_err("EC2_AMI_NAME_FILTER not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("ami_name_filter={}", ami_name_filter));

        let ec2_profile_name = dotenvy::var("EC2_PROFILE_NAME")
            .wrap_err("EC2_PROFILE_NAME not set (check environment or .env)")?;
        args.push("-var".to_string());
        args.push(format!("ec2_profile_name={}", ec2_profile_name));

        if let Some(size) = node_size {
            args.push("-var".to_string());
            args.push(format!("node_size={size}"));
        }

        if let Some(size) = cc_size {
            args.push("-var".to_string());
            args.push(format!("cc_size={size}"));
        }

        Ok(args)
    }
}
