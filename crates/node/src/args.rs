// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

//! Default CLI args for the Reth `node` subcommand.
//!
//! Sets Arc's default values on the node subcommand's args.
//! User-passed flags override these defaults as usual.

use std::collections::HashMap;

use clap::Command;

/// Built-in Arc default flags for the `node` subcommand (used to derive arg default overrides).
pub(crate) const ARC_DEFAULT_NODE_FLAGS: &[&str] = &[
    "--engine.memory-block-buffer-target=0",
    "--engine.persistence-threshold=0",
    "--rpc.pending-block=none",
    "--rpc.txfeecap=1000",
];

// Subcommand name for the `node` subcommand.
const NODE_SUBCOMMAND: &str = "node";

/// Patches the node subcommand's args so their default values are Arc's.
pub fn patch_node_command_defaults(root_cmd: Command) -> Command {
    let overrides: HashMap<_, _> = arc_node_arg_default_overrides().collect();
    root_cmd.mut_subcommand(NODE_SUBCOMMAND, |subcmd| {
        subcmd.mut_args(|arg| {
            if let Some(long_name) = arg.get_long() {
                if let Some(&arc_val) = overrides.get(long_name) {
                    return arg.default_value(arc_val);
                }
            }
            arg
        })
    })
}

/// Returns `(long_name, value)` for each flag in `ARC_DEFAULT_NODE_FLAGS`.
/// Flags with `=` use that value; flags without (e.g. `--http`, `--ws`) get `"true"`.
fn arc_node_arg_default_overrides() -> impl Iterator<Item = (&'static str, &'static str)> {
    ARC_DEFAULT_NODE_FLAGS.iter().filter_map(|f| {
        let s = f.strip_prefix("--")?;
        let (name, val) = s.split_once('=').unwrap_or((s, "true"));
        Some((name, val))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, Command};
    use std::collections::HashMap;

    #[test]
    fn test_arg_default_overrides_derived_from_const() {
        let overrides: HashMap<_, _> = arc_node_arg_default_overrides().collect();
        let mut expected = HashMap::new();
        expected.insert("engine.memory-block-buffer-target", "0");
        expected.insert("engine.persistence-threshold", "0");
        expected.insert("rpc.pending-block", "none");
        expected.insert("rpc.txfeecap", "1000");

        assert_eq!(
            overrides, expected,
            "Overrides should match expected defaults"
        );
    }

    #[test]
    fn test_patch_node_command_defaults_applies_default() {
        let cmd = Command::new("bin").subcommand(
            Command::new(NODE_SUBCOMMAND).arg(
                Arg::new("rpc.txfeecap")
                    .long("rpc.txfeecap")
                    .default_value("1.0"),
            ),
        );
        let patched = patch_node_command_defaults(cmd);
        let args_matches = patched.get_matches_from(["bin", "node"]);
        let node_matches = args_matches.subcommand_matches(NODE_SUBCOMMAND).unwrap();

        assert_eq!(
            node_matches.get_one::<String>("rpc.txfeecap").cloned(),
            Some("1000".to_string()),
            "Arc default overrides Reth default"
        );
    }
}
