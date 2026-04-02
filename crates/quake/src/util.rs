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

use color_eyre::eyre::{bail, Result};
use futures_util::future::join_all;
use indexmap::IndexMap;
use std::future::Future;
use toml::Value;

use crate::manifest::{Node, NodeType};

/// Merges two toml values into a single one.
/// First value is the base value, second value will overwrite the first value when
/// required.
pub fn merge_toml_values(value: Value, other: Value) -> Result<Value> {
    match (value, other) {
        (Value::Table(mut existing), Value::Table(inner)) => {
            for (name, value) in inner {
                if let Some(prev) = existing.remove(&name) {
                    existing.insert(name, merge_toml_values(prev, value)?);
                } else {
                    existing.insert(name, value);
                }
            }
            Ok(Value::Table(existing))
        }
        (v, o) if v.type_str() == o.type_str() => Ok(o),
        (v, o) => bail!(
            "Failed to merge toml values: {} and {}",
            v.type_str(),
            o.type_str()
        ),
    }
}

/// Assign "Validators" / "Non-Validators" group labels to nodes based on the
/// manifest. Matching uses exact name comparison to avoid false positives
/// (e.g. "validator1" must not match "validator10").
pub fn assign_node_groups<'a>(
    nodes: impl Iterator<Item = (&'a str, &'a mut Option<String>)>,
    manifest_nodes: &IndexMap<String, Node>,
) {
    for (node_name, group) in nodes {
        if let Some((_, manifest_node)) = manifest_nodes
            .iter()
            .find(|(name, _)| node_name == name.as_str())
        {
            *group = Some(match manifest_node.node_type {
                NodeType::Validator => "Validators".to_string(),
                NodeType::NonValidator => "Non-Validators".to_string(),
            });
        }
    }
}

/// Execute an async operation on multiple items in parallel.
pub async fn in_parallel<T, R, F, Fut>(items: &[&T], op: F) -> Vec<R>
where
    T: Clone + Send + 'static,
    R: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: Future<Output = R> + Send + 'static,
{
    let futures = items.iter().map(|item| op((*item).clone()));
    join_all(futures).await
}

/// Execute an async operation on multiple tuples in parallel.
pub async fn in_parallel_tuples<S, T, R, F, Fut>(items: &[(S, T)], op: F) -> Vec<R>
where
    S: Clone + Send + 'static,
    T: Clone + Send + 'static,
    R: Send + 'static,
    F: Fn(S, T) -> Fut,
    Fut: Future<Output = R> + Send + 'static,
{
    let futures = items.iter().map(|(s, t)| op((*s).clone(), (*t).clone()));
    join_all(futures).await
}
