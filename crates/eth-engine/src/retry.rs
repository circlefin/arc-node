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

use std::time::Duration;

// A backoff policy that never retries, used as the default.
#[derive(Clone, Default)]
pub struct NoRetry;

impl Iterator for NoRetry {
    type Item = Duration;

    fn next(&mut self) -> Option<Self::Item> {
        // Never yields a duration, so the operation is attempted only once.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backon::Retryable;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_no_retry() {
        let attempts = AtomicUsize::new(0);
        let operation = || async {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>("operation failed")
        };

        let result = operation
            .retry(NoRetry)
            .notify(|e, dur| {
                panic!(
                    "This should not be printed, as NoRetry does not retry. Error: {e}, Duration: {dur:?}",
                );
            })
            .await;

        assert!(result.is_err());

        // Ensure the operation was attempted only once.
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
