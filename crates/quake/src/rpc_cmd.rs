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

//! `quake rpc` — call EL JSON-RPC or CL REST endpoints on one or more nodes.
//!
//! The module is structured as a thin CLI on top of reusable fan-out helpers
//! (`fanout_el`, `fanout_cl`, `fetch_cl_catalog`) shared with the MCP server.
//! See `crates/quake/README.md` for the full command reference.

use clap::{Args, ValueEnum};
use color_eyre::eyre::{bail, eyre, Result, WrapErr};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Method};
use serde_json::Value;
use std::str::FromStr;
use std::time::Duration;
use tracing::warn;
use url::Url;

use crate::node::NodeName;
use crate::parse_duration;
use crate::util;

/// Reth's published JSON-RPC reference. Surfaced from `quake rpc list` (and
/// the MCP `rpc_list` tool) because the EL side does not expose a per-method
/// introspection endpoint.
pub(crate) const RETH_JSONRPC_DOCS_URL: &str = "https://reth.rs/jsonrpc/intro";

// ---------- CLI args --------------------------------------------------------

#[derive(Args)]
pub(crate) struct ElArgs {
    /// JSON-RPC method name, e.g. `admin_clearTxpool`.
    pub method: String,

    /// Positional slots after the method: `[target] [params...]`.
    ///
    /// - Zero entries: target defaults to ALL_NODES, no params.
    /// - One entry: target only (no params).
    /// - Two or more: first entry is the target, the rest are params
    ///   (auto-promoted via `serde_json::from_str`; otherwise quoted as string).
    ///
    /// The target is a comma-separated list of node names or manifest node
    /// groups (e.g. `validator1,ALL_VALIDATORS`). To pass params with the
    /// default target set, write `ALL_NODES` explicitly.
    #[arg(value_name = "TARGET|PARAM")]
    pub positional: Vec<String>,

    /// Raw JSON array of params. Mutually exclusive with positional params.
    #[arg(long, value_name = "JSON")]
    pub raw: Option<String>,

    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Args)]
pub(crate) struct ClArgs {
    /// REST path. A leading `/` is optional and prepended automatically.
    /// May include a query string. Examples: `/status`, `commit`,
    /// `/commit?height=42`.
    pub path: String,

    /// Comma-separated targets (node names or manifest groups). Defaults to
    /// all consensus-enabled nodes.
    #[arg(value_name = "TARGET")]
    pub target: Option<String>,

    /// HTTP method (GET, POST, DELETE, PUT, PATCH).
    #[arg(long, default_value = "GET")]
    pub method: String,

    /// JSON body for POST/DELETE/PUT/PATCH.
    #[arg(long, value_name = "JSON")]
    pub body: Option<String>,

    #[command(flatten)]
    pub common: CommonArgs,
}

impl ElArgs {
    /// Split positional args into `(target, params)` using the rule that the
    /// target slot must be present whenever params are present.
    pub(crate) fn parse_positionals(&self) -> Result<(Option<String>, Vec<String>)> {
        match self.positional.len() {
            0 => Ok((None, Vec::new())),
            1 => Ok((Some(self.positional[0].clone()), Vec::new())),
            _ => {
                if self.raw.is_some() {
                    bail!(
                        "--raw cannot be combined with positional params. \
                         Pass only `<target>` followed by --raw '<json>'"
                    );
                }
                Ok((
                    Some(self.positional[0].clone()),
                    self.positional[1..].to_vec(),
                ))
            }
        }
    }
}

#[derive(Args, Clone)]
pub(crate) struct CommonArgs {
    /// Per-node request timeout. Default 10s. Accepts e.g. `30s`, `1m`.
    #[arg(long, default_value = "10s", value_parser = parse_duration)]
    pub timeout: Duration,

    /// Number of retries per node on transport failure.
    #[arg(long, default_value = "0")]
    pub retries: u32,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Newline-delimited JSON, one object per node.
    Json,
    /// Two-column NODE | RESULT table, sorted by node name.
    Table,
    /// Result value only. Requires a single target.
    Raw,
}

// ---------- main functions ------------------------------------------------------------

pub(crate) async fn run_list(node: NodeName, base_url: Url) -> Result<()> {
    let catalog = fetch_cl_catalog(node, base_url, Duration::from_secs(10)).await?;

    println!(
        "Consensus Layer (REST) — endpoints on {} ({}):",
        catalog.node, catalog.url
    );
    match catalog.endpoints {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(err) => {
            warn!(%err, "Failed to fetch CL endpoint catalog");
            println!("  (unreachable: {err})");
        }
    }

    println!();
    println!("Execution Layer (JSON-RPC) — methods documented at:");
    println!("  {RETH_JSONRPC_DOCS_URL}");
    Ok(())
}

pub(crate) async fn run_el(node_urls: Vec<(NodeName, Url)>, args: ElArgs) -> Result<()> {
    let (_target, params) = args.parse_positionals()?;

    let params_value = match args.raw {
        Some(raw) => serde_json::from_str::<Value>(&raw)
            .wrap_err("--raw must be a valid JSON value (typically an array)")?,
        None => Value::Array(params.into_iter().map(promote_param).collect()),
    };

    let outputs = fanout_el(
        node_urls,
        &args.method,
        params_value,
        args.common.timeout,
        args.common.retries,
    )
    .await?;

    render_and_exit(outputs, args.common.format)
}

/// Matches `cast rpc` param semantics: JSON literals keep their type, anything
/// else is treated as a string.
fn promote_param(raw: String) -> Value {
    serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw))
}

pub(crate) async fn run_cl(node_urls: Vec<(NodeName, Url)>, args: ClArgs) -> Result<()> {
    let method = Method::from_str(&args.method.to_uppercase())
        .map_err(|_| eyre!("Invalid HTTP method '{}'", args.method))?;

    let outputs = fanout_cl(
        node_urls,
        &args.path,
        &method,
        args.body.as_deref(),
        args.common.timeout,
        args.common.retries,
    )
    .await?;

    render_and_exit(outputs, args.common.format)
}

// ---------- output rendering (CLI only) -------------------------------------

/// Print collected outputs in the requested format and return an error if any
/// node failed so the process exits non-zero.
fn render_and_exit(
    outputs: Vec<(NodeName, Result<Value, String>)>,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Raw => return render_raw(outputs),
        OutputFormat::Json => {
            for (node, result) in &outputs {
                println!(
                    "{}",
                    serde_json::to_string(&node_output_json(node, result))?
                );
            }
        }
        OutputFormat::Table => print_table(&outputs),
    }

    let failed = outputs.iter().filter(|(_, r)| r.is_err()).count();
    if failed > 0 {
        bail!("{failed}/{} node(s) failed", outputs.len());
    }
    Ok(())
}

/// Build the per-node JSON envelope used by `--format json` and the MCP
/// equivalents. Exactly one of `result` / `error` is present, mirroring the
/// `Result` it came from.
fn node_output_json(node: &str, result: &Result<Value, String>) -> Value {
    match result {
        Ok(v) => serde_json::json!({"node": node, "result": v}),
        Err(e) => serde_json::json!({"node": node, "error": e}),
    }
}

/// Render the single-target `--format raw` case. The array conversion encodes
/// the "exactly one target" invariant in the type system, so this function
/// cannot reach an empty-or-multi state by mistake.
fn render_raw(outputs: Vec<(NodeName, Result<Value, String>)>) -> Result<()> {
    let len = outputs.len();
    let array: [(NodeName, Result<Value, String>); 1] = outputs
        .try_into()
        .map_err(|_| eyre!("--format raw requires exactly one target node (got {len})"))?;
    let [(_, result)] = array;
    match result {
        Ok(v) => {
            println!("{}", value_to_string(&v));
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
}

/// Strip surrounding quotes when the result is already a JSON string scalar.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn print_table(outputs: &[(NodeName, Result<Value, String>)]) {
    let node_w = outputs
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{:<node_w$}  RESULT", "NODE", node_w = node_w);
    println!("{:<node_w$}  ------", "----", node_w = node_w);
    for (node, result) in outputs {
        let cell = match result {
            Ok(v) => value_to_string(v),
            Err(e) => format!("ERROR: {e}"),
        };
        println!("{node:<node_w$}  {cell}", node_w = node_w);
    }
}

// ============================================================================
// Reusable fan-out helpers (shared with the MCP server)
// ============================================================================

/// Fan out a JSON-RPC call to the given EL nodes in parallel. Results are
/// sorted by node name. Each per-node entry is `Ok(result_value)` on success
/// or `Err(message)` for transport, parse, or JSON-RPC errors. Callers
/// typically obtain `node_urls` via `NodesMetadata::resolve_el_targets`.
pub(crate) async fn fanout_el(
    node_urls: Vec<(NodeName, Url)>,
    method: &str,
    params: Value,
    timeout: Duration,
    retries: u32,
) -> Result<Vec<(NodeName, Result<Value, String>)>> {
    let method = method.to_string();
    let shared_client = Client::new();
    let mut outputs = util::in_parallel_tuples(&node_urls, move |name, url| {
        let method = method.clone();
        let params = params.clone();
        let client = crate::rpc::RpcClient::with_client(shared_client.clone(), url, timeout);
        async move {
            let result = client
                .rpc_request::<Value>(&method, params, retries)
                .await
                .map_err(|e| e.to_string());
            (name, result)
        }
    })
    .await;
    outputs.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(outputs)
}

/// Fan out a REST call to the given CL nodes in parallel.
///
/// `path` may omit the leading `/` (it is normalized internally). `body` is
/// sent verbatim as the HTTP body when supplied. Results are sorted by node
/// name; each entry parses the response as JSON when possible, falling back
/// to a JSON string of the raw body. Callers typically obtain `node_urls`
/// via `NodesMetadata::resolve_cl_targets`.
pub(crate) async fn fanout_cl(
    node_urls: Vec<(NodeName, Url)>,
    path: &str,
    http_method: &Method,
    body: Option<&str>,
    timeout: Duration,
    retries: u32,
) -> Result<Vec<(NodeName, Result<Value, String>)>> {
    let normalized_path = normalize_cl_path(path);
    let body = body.map(str::to_string);
    let http_method = http_method.clone();
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .wrap_err("Failed to build HTTP client")?;
    let mut outputs = util::in_parallel_tuples(&node_urls, move |name, base| {
        let path = normalized_path.clone();
        let body = body.clone();
        let http_method = http_method.clone();
        let client = client.clone();
        async move {
            let result = call_cl_one(&client, base, &http_method, &path, body, retries).await;
            (name, result)
        }
    })
    .await;
    outputs.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(outputs)
}

/// CL endpoint catalog fetched from a single node's `GET /` index.
pub(crate) struct ClCatalog {
    pub node: NodeName,
    pub url: Url,
    /// Parsed JSON catalog, or an error message if the fetch/parse failed.
    pub endpoints: std::result::Result<Value, String>,
}

/// Fetch the CL endpoint catalog from the given consensus-enabled node. The
/// catalog is a function of each node's build/config, so on a healthy network
/// every consensus-enabled node returns the same response; picking a specific
/// node would only matter during version skew. Callers therefore typically
/// pass the first consensus-enabled node from the manifest.
pub(crate) async fn fetch_cl_catalog(
    node: NodeName,
    base_url: Url,
    timeout: Duration,
) -> Result<ClCatalog> {
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .wrap_err("Failed to build HTTP client")?;

    // The catalog is served at `/` on the CL REST API. In remote mode the SSM
    // proxy routes `/<node>/cl(/<path>)?` to the upstream and, when path is
    // empty, passes the original URI through unchanged. Forcing a trailing
    // slash makes the proxy rewrite to `GET /` so the upstream returns the
    // catalog instead of a 404.
    let catalog_url = format!("{}/", base_url.as_str().trim_end_matches('/'));

    let endpoints = match client.get(&catalog_url).send().await {
        Ok(resp) => {
            let status = resp.status();
            match resp.text().await {
                Ok(text) if status.is_success() => Ok(serde_json::from_str::<Value>(&text)
                    .unwrap_or_else(|_| Value::String(text.clone()))),
                Ok(text) => {
                    let body_preview = text.trim();
                    if body_preview.is_empty() {
                        Err(format!("HTTP {status}"))
                    } else {
                        Err(format!("HTTP {status}: {body_preview}"))
                    }
                }
                Err(e) => Err(format!("response read failed: {e}")),
            }
        }
        Err(e) => Err(format!("request failed: {e}")),
    };

    Ok(ClCatalog {
        node,
        url: base_url,
        endpoints,
    })
}

// ---------- internal: per-node HTTP calls -----------------------------------

async fn call_cl_one(
    client: &Client,
    base: Url,
    method: &Method,
    path: &str,
    body: Option<String>,
    retries: u32,
) -> std::result::Result<Value, String> {
    let full_url = format!("{}{path}", base.as_str().trim_end_matches('/'));
    let mut last_err: Option<String> = None;
    // `0..=retries` runs at least once (when retries == 0), so by exit
    // `last_err` is always `Some` for the failing path.
    for _ in 0..=retries {
        let mut req = client.request(method.clone(), &full_url);
        if let Some(b) = &body {
            req = req.header(CONTENT_TYPE, "application/json").body(b.clone());
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let text = match resp.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        last_err = Some(format!("response read failed: {e}"));
                        continue;
                    }
                };
                let value = serde_json::from_str::<Value>(&text)
                    .unwrap_or_else(|_| Value::String(text.clone()));
                if status.is_success() {
                    return Ok(value);
                }
                return Err(format!("HTTP {status}: {value}"));
            }
            Err(e) => last_err = Some(format!("request failed: {e}")),
        }
    }
    Err(last_err.expect("0..=retries iterates at least once"))
}

/// Accepts both `/consensus-state` and `consensus-state`.
fn normalize_cl_path(raw: &str) -> String {
    if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("/{raw}")
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_el_args(positional: Vec<String>, raw: Option<String>) -> ElArgs {
        ElArgs {
            method: String::new(),
            positional,
            raw,
            common: CommonArgs {
                timeout: Duration::ZERO,
                retries: 0,
                format: OutputFormat::Json,
            },
        }
    }

    #[test]
    fn parse_positionals_zero_args_defaults_to_all_nodes() {
        let args = make_el_args(vec![], None);
        let (target, params) = args.parse_positionals().unwrap();
        assert!(target.is_none());
        assert!(params.is_empty());
    }

    #[test]
    fn parse_positionals_one_arg_is_target() {
        let args = make_el_args(vec!["validator1".to_string()], None);
        let (target, params) = args.parse_positionals().unwrap();
        assert_eq!(target.as_deref(), Some("validator1"));
        assert!(params.is_empty());
    }

    #[test]
    fn parse_positionals_multi_arg_splits_target_and_params() {
        let args = make_el_args(
            vec![
                "validator1".to_string(),
                "0xabc".to_string(),
                "latest".to_string(),
            ],
            None,
        );
        let (target, params) = args.parse_positionals().unwrap();
        assert_eq!(target.as_deref(), Some("validator1"));
        assert_eq!(params, vec!["0xabc".to_string(), "latest".to_string()]);
    }

    #[test]
    fn parse_positionals_rejects_raw_plus_positional_params() {
        let args = make_el_args(
            vec!["validator1".to_string(), "0xabc".to_string()],
            Some("[]".to_string()),
        );
        let err = args.parse_positionals().unwrap_err();
        assert!(err.to_string().contains("--raw"));
    }

    #[test]
    fn parse_positionals_allows_raw_with_target_only() {
        let args = make_el_args(vec!["validator1".to_string()], Some("[]".to_string()));
        let (target, params) = args.parse_positionals().unwrap();
        assert_eq!(target.as_deref(), Some("validator1"));
        assert!(params.is_empty());
    }

    #[test]
    fn value_to_string_strips_quotes_only_for_string_scalars() {
        assert_eq!(value_to_string(&json!("latest")), "latest");
        assert_eq!(value_to_string(&json!(42)), "42");
        assert_eq!(value_to_string(&json!(true)), "true");
        assert_eq!(value_to_string(&Value::Null), "null");
        assert_eq!(value_to_string(&json!([1, 2])), "[1,2]");
        assert_eq!(value_to_string(&json!({"k": "v"})), "{\"k\":\"v\"}");
    }

    #[test]
    fn node_output_json_emits_result_or_error_exclusively() {
        let ok = node_output_json("validator1", &Ok(json!(42)));
        assert_eq!(ok, json!({"node": "validator1", "result": 42}));
        assert!(ok.get("error").is_none());

        let err = node_output_json("validator2", &Err("boom".to_string()));
        assert_eq!(err, json!({"node": "validator2", "error": "boom"}));
        assert!(err.get("result").is_none());
    }

    #[test]
    fn normalize_cl_path_prepends_slash_when_missing() {
        assert_eq!(normalize_cl_path("consensus-state"), "/consensus-state");
        assert_eq!(normalize_cl_path("commit?height=42"), "/commit?height=42");
    }

    #[test]
    fn normalize_cl_path_preserves_existing_slash() {
        assert_eq!(normalize_cl_path("/consensus-state"), "/consensus-state");
        assert_eq!(normalize_cl_path("/"), "/");
    }
}
