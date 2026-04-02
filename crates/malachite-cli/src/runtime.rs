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

//! Multithreaded runtime builder.

use malachitebft_config::RuntimeConfig;
use std::io::Result;
use tokio::runtime::{Builder as RtBuilder, Runtime};

pub fn build_runtime(cfg: RuntimeConfig) -> Result<Runtime> {
    let mut builder = match cfg {
        RuntimeConfig::SingleThreaded => RtBuilder::new_current_thread(),
        RuntimeConfig::MultiThreaded { worker_threads } => {
            let mut builder = RtBuilder::new_multi_thread();
            if worker_threads > 0 {
                builder.worker_threads(worker_threads);
            }
            builder
        }
    };

    builder.enable_all().build()
}
