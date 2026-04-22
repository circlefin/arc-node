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

use std::path::PathBuf;

use crate::error::EvmSpecsTestError;
use crate::result::RunStatus;

/// Thin CLI wrapper over the ARC-backed statetest runner harness.
pub fn run(
    path: PathBuf,
    filter_name: Option<String>,
    strict_exit: bool,
    trace: bool,
    json_outcome: bool,
) -> Result<RunStatus, EvmSpecsTestError> {
    crate::runner::run(path, filter_name, strict_exit, trace, json_outcome)
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::error::EvmSpecsTestError;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn run_forwards_runner_no_json_files_error() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("arc_evm_specs_cmd_empty_{nonce}"));
        std::fs::create_dir_all(&root).expect("temp dir should be created");

        let err = run(root.clone(), None, false, false, false)
            .expect_err("empty directory should fail through wrapper");

        assert!(matches!(
            err,
            EvmSpecsTestError::NoJsonFiles { path } if path == root.display().to_string()
        ));

        std::fs::remove_dir_all(root).expect("temp dir should be removed");
    }
}
