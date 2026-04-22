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

/// Narrow compatibility shim for fixture fields that current typed upstream
/// parsing does not accept yet.
///
/// This intentionally rewrites only the supported top-level `state` field and
/// direct `post.<fork>[]` entries. Nested objects are left untouched so schema
/// drift surfaces instead of being recursively mutated into shape.
pub(crate) fn strip_unsupported_fields(value: &mut serde_json::Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };

    for test_unit in root.values_mut() {
        let Some(test_map) = test_unit.as_object_mut() else {
            continue;
        };

        if !test_map.contains_key("postState")
            && let Some(state) = test_map.remove("state")
        {
            test_map.insert("postState".to_string(), state);
        }

        if let Some(post) = test_map.get_mut("post") {
            strip_unsupported_fields_in_post(post);
        }
    }
}

fn strip_unsupported_fields_in_post(post: &mut serde_json::Value) {
    let Some(forks) = post.as_object_mut() else {
        return;
    };

    for entries in forks.values_mut() {
        let Some(items) = entries.as_array_mut() else {
            continue;
        };

        for item in items {
            let Some(map) = item.as_object_mut() else {
                continue;
            };

            if !map.contains_key("postState")
                && let Some(state) = map.remove("state")
            {
                map.insert("postState".to_string(), state);
            }
            map.remove("receipt");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip_unsupported_fields;

    #[test]
    fn strip_unsupported_fields_removes_receipt_and_sets_post_state() {
        let mut value = serde_json::json!({
            "suite": {
                "receipt": { "status": "0x1" },
                "state": { "0x1": { "balance": "0x01" } },
                "post": {
                    "Cancun": [
                        {
                            "receipt": { "status": "0x1" }
                        }
                    ]
                }
            }
        });
        strip_unsupported_fields(&mut value);
        let suite = value
            .get("suite")
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(suite.contains_key("receipt"));
        assert!(!suite.contains_key("state"));
        assert!(suite.contains_key("postState"));
        let post_entry = suite
            .get("post")
            .and_then(|v| v.get("Cancun"))
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(!post_entry.contains_key("receipt"));
    }

    #[test]
    fn strip_unsupported_fields_preserves_existing_post_state() {
        let mut value = serde_json::json!({
            "suite": {
                "state": { "old": true },
                "postState": { "kept": true },
                "post": {
                    "Prague": [
                        {
                            "receipt": { "status": "0x1" },
                            "nested": {
                                "receipt": { "status": "0x1" }
                            }
                        }
                    ]
                }
            }
        });

        strip_unsupported_fields(&mut value);
        let suite = value.get("suite").unwrap();

        assert_eq!(
            suite.get("postState").unwrap(),
            &serde_json::json!({ "kept": true })
        );
        assert_eq!(
            suite.get("state").unwrap(),
            &serde_json::json!({ "old": true })
        );
        let post_entry = suite
            .get("post")
            .and_then(|v| v.get("Prague"))
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(serde_json::Value::as_object)
            .unwrap();
        assert!(!post_entry.contains_key("receipt"));
        assert!(suite
            .get("post")
            .and_then(|v| v.get("Prague"))
            .and_then(serde_json::Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|entry| entry.get("nested"))
            .and_then(serde_json::Value::as_object)
            .is_some_and(|nested| nested.contains_key("receipt")));
    }
}
