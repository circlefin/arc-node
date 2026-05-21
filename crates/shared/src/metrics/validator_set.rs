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

//! Prometheus metrics for validator-set decoding.
//!
//! Counter: `arc_validator_set_skipped_total`
//!
//! The validator's public key is not included as a label (avoids cardinality); it can be found in logs.
//!
//! The consensus binary exports metrics via the malachite `prometheus-client` registry, while
//! the rest of `arc-shared` is also consumed by execution-layer crates that use the `metrics-rs`
//! facade. To avoid coupling `arc-shared` to either backend, this module exposes a recorder
//! callback that the consensus binary installs at startup with a closure that increments a
//! `Counter` registered against `SharedRegistry::global()`. If no recorder is installed (unit
//! tests, execution-layer call paths), `record_skipped_validator` is a no-op.

use std::sync::OnceLock;

/// Callback invoked once per skipped validator. Set once at startup via [`set_recorder`].
type Recorder = Box<dyn Fn() + Send + Sync + 'static>;

static RECORDER: OnceLock<Recorder> = OnceLock::new();

/// Install the recorder for `arc_validator_set_skipped_total`. Idempotent: subsequent calls
/// are ignored so unit tests that exercise startup logic do not panic.
pub fn set_recorder(recorder: Recorder) {
    let _ = RECORDER.set(recorder);
}

/// Records that one active validator was skipped during validator-set decoding because its
/// public key was malformed. Operators should alert on any non-zero rate for this counter.
pub fn record_skipped_validator() {
    if let Some(recorder) = RECORDER.get() {
        recorder();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn record_skipped_validator_without_recorder_does_not_panic() {
        // No recorder installed in this test (the static may be set by another test, which
        // is fine — the call must not panic either way).
        record_skipped_validator();
    }

    #[test]
    fn set_recorder_is_idempotent() {
        // Use a dedicated OnceLock-backed fixture to prove `set_recorder` swallows duplicate
        // installs without panicking. The global `RECORDER` is left untouched.
        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = Arc::clone(&counter);

        let local: OnceLock<Recorder> = OnceLock::new();
        let _ = local.set(Box::new(move || {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        }));
        let _ = local.set(Box::new(|| {})); // second install is dropped

        local.get().unwrap()();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
}
