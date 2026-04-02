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

//! Malachite CL store.db (redb) inspection and health checks.

use std::fmt;
use std::path::Path;

use arc_consensus_db::{
    CERTIFICATES_TABLE, DECIDED_BLOCKS_TABLE, INVALID_PAYLOADS_TABLE, MISBEHAVIOR_EVIDENCE_TABLE,
    PENDING_PROPOSAL_PARTS_TABLE, PROPOSAL_MONITOR_DATA_TABLE, UNDECIDED_BLOCKS_TABLE,
};
use color_eyre::eyre::{self, Result};
use redb::{ReadableTable, ReadableTableMetadata, TableHandle};

use crate::types::{CheckResult, Report};

/// Statistics for a single height-keyed table.
#[derive(Debug, Clone)]
pub struct HeightTableInfo {
    pub name: String,
    pub records: u64,
    pub min_height: Option<u64>,
    pub max_height: Option<u64>,
}

/// Statistics for a composite-keyed table (count only).
#[derive(Debug, Clone)]
pub struct CompositeTableInfo {
    pub name: String,
    pub records: u64,
}

/// Collected statistics for a CL store.db.
#[derive(Debug, Clone)]
pub struct StoreInfo {
    pub path: String,
    pub size_bytes: u64,
    pub height_tables: Vec<HeightTableInfo>,
    pub composite_tables: Vec<CompositeTableInfo>,
    pub unknown_tables: Vec<String>,
}

impl StoreInfo {
    /// Look up a height-keyed table by name.
    pub fn height_table(&self, name: &str) -> Option<&HeightTableInfo> {
        self.height_tables.iter().find(|t| t.name == name)
    }
}

/// Collect statistics from a CL store.db file.
pub fn collect_store_info(store_path: &Path) -> Result<StoreInfo> {
    let metadata = std::fs::metadata(store_path)
        .map_err(|e| eyre::eyre!("Cannot stat {}: {e}", store_path.display()))?;

    let db =
        redb::Database::open(store_path).map_err(|e| eyre::eyre!("Failed to open redb: {e}"))?;
    let tx = db
        .begin_read()
        .map_err(|e| eyre::eyre!("Failed to begin read transaction: {e}"))?;

    let mut height_tables = Vec::new();

    macro_rules! collect_height_table {
        ($name:expr, $def:expr) => {
            match tx.open_table($def) {
                Ok(table) => {
                    let records = table.len().unwrap_or(0);
                    let min_height = table
                        .first()
                        .ok()
                        .flatten()
                        .map(|(k, _)| k.value().as_u64());
                    let max_height = table.last().ok().flatten().map(|(k, _)| k.value().as_u64());
                    height_tables.push(HeightTableInfo {
                        name: $name.to_string(),
                        records,
                        min_height,
                        max_height,
                    });
                }
                Err(_) => {
                    height_tables.push(HeightTableInfo {
                        name: $name.to_string(),
                        records: 0,
                        min_height: None,
                        max_height: None,
                    });
                }
            }
        };
    }

    collect_height_table!("certificates", CERTIFICATES_TABLE);
    collect_height_table!("decided_blocks", DECIDED_BLOCKS_TABLE);
    collect_height_table!("misbehavior_evidence", MISBEHAVIOR_EVIDENCE_TABLE);
    collect_height_table!("invalid_payloads", INVALID_PAYLOADS_TABLE);
    collect_height_table!("proposal_monitor_data", PROPOSAL_MONITOR_DATA_TABLE);

    let mut composite_tables = Vec::new();

    macro_rules! collect_composite_table {
        ($name:expr, $def:expr) => {
            match tx.open_table($def) {
                Ok(table) => {
                    composite_tables.push(CompositeTableInfo {
                        name: $name.to_string(),
                        records: table.len().unwrap_or(0),
                    });
                }
                Err(_) => {
                    composite_tables.push(CompositeTableInfo {
                        name: $name.to_string(),
                        records: 0,
                    });
                }
            }
        };
    }

    collect_composite_table!("undecided_blocks", UNDECIDED_BLOCKS_TABLE);
    collect_composite_table!("pending_proposal_parts", PENDING_PROPOSAL_PARTS_TABLE);

    let known = [
        "certificates",
        "decided_blocks",
        "misbehavior_evidence",
        "invalid_payloads",
        "proposal_monitor_data",
        "undecided_blocks",
        "pending_proposal_parts",
    ];
    let all_tables = tx.list_tables().map_err(|e| eyre::eyre!("{e}"))?;
    let unknown_tables: Vec<String> = all_tables
        .into_iter()
        .map(|h| h.name().to_string())
        .filter(|name| !known.contains(&name.as_str()))
        .collect();

    Ok(StoreInfo {
        path: store_path.display().to_string(),
        size_bytes: metadata.len(),
        height_tables,
        composite_tables,
        unknown_tables,
    })
}

/// Format `StoreInfo` as a human-readable table (for `quake info store`).
impl fmt::Display for StoreInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "  Store: {} ({:.2} MB on disk)",
            self.path,
            self.size_bytes as f64 / (1024.0 * 1024.0)
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "  {:<25} {:>8}   {:>10}   {:>10}",
            "Table", "Records", "Min Key", "Max Key"
        )?;
        writeln!(f, "  {}", "-".repeat(60))?;

        for t in &self.height_tables {
            let min = t
                .min_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "-".into());
            let max = t
                .max_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "-".into());
            writeln!(
                f,
                "  {:<25} {:>8}   {:>10}   {:>10}",
                t.name, t.records, min, max
            )?;
        }

        for t in &self.composite_tables {
            writeln!(f, "  {:<25} {:>8}", t.name, t.records)?;
        }

        for name in &self.unknown_tables {
            writeln!(f, "  {name:<25}   (untyped)")?;
        }

        writeln!(f)?;
        Ok(())
    }
}

/// Check that pruned tables satisfy the expected pruning window.
///
/// For each height-keyed table that has data, verifies that the number of
/// records does not exceed `max_records`. The `certificates` table is
/// used as the primary indicator since it is always pruned.
pub fn check_store_pruning(info: &StoreInfo, max_records: u64) -> Report {
    let pruned_tables = ["certificates"];

    let checks: Vec<CheckResult> = pruned_tables
        .iter()
        .filter_map(|&name| info.height_table(name))
        .map(|table| {
            let passed = table.records <= max_records;
            let range = match (table.min_height, table.max_height) {
                (Some(min), Some(max)) => format!("{min}..{max}"),
                _ => "-".into(),
            };
            let message = if passed {
                format!(
                    "{}: {} records (range {}) within limit {}",
                    table.name, table.records, range, max_records
                )
            } else {
                format!(
                    "{}: {} records (range {}) exceeds limit {}",
                    table.name, table.records, range, max_records
                )
            };
            CheckResult {
                name: table.name.clone(),
                passed,
                message,
            }
        })
        .collect();

    Report { checks }
}
