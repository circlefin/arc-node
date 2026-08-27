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

use alloy_primitives::Bytes;
use eyre::{eyre, Result};
use serde_json::{json, Value};
use std::{cell::Cell, time::Duration};

pub(crate) fn send_raw_tx_body(raw_tx: &Bytes, id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "eth_sendRawTransaction", "params": [raw_tx]})
}

/// Submits raw EIP-2718 transactions to the target node's eth RPC over JSON-RPC.
pub(crate) struct TxSubmitter {
    client: reqwest::Client,
    url: String,
    next_id: Cell<u64>,
}

impl TxSubmitter {
    pub(crate) fn new(url: String, timeout_ms: u64) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()?;
        Ok(Self {
            client,
            url,
            next_id: Cell::new(1),
        })
    }

    /// Submit one raw EIP-2718 transaction. Returns the tx hash on success; Err carries the
    /// JSON-RPC error.
    pub(crate) async fn send_raw_transaction(&self, raw_tx: &Bytes) -> Result<String> {
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));
        let resp: Value = self
            .client
            .post(&self.url)
            .json(&send_raw_tx_body(raw_tx, id))
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            return Err(eyre!("eth_sendRawTransaction error: {err}"));
        }
        Ok(resp
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;

    #[test]
    fn builds_send_raw_tx_request_body() {
        let raw = Bytes::from(vec![0x02, 0xaa, 0xbb]);
        let body = send_raw_tx_body(&raw, 7);
        assert_eq!(body["method"], "eth_sendRawTransaction");
        assert_eq!(body["id"], 7);
        assert_eq!(body["params"][0], "0x02aabb");
    }
}
