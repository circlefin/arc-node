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

use super::helpers::duration_to_ms;
use eyre::{bail, Context};
use serde::Serialize;
use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) const COMBINED_LATENCY_FILE_NAME: &str = "combined_latency.csv";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CombinedLatencyRow {
    pub block_number: u64,
    pub block_hash: String,
    pub tx_count: u64,
    pub gas_used: u64,
    pub new_payload_ms: f64,
    pub fcu_ms: f64,
    pub total_ms: f64,
    pub elapsed_ms: f64,
    pub mgas_per_s: f64,
    pub tx_per_s: f64,
    pub cumulative_mgas_per_s: f64,
    pub cumulative_tx_per_s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SummaryRow {
    pub mode: String,
    pub samples: u64,
    pub total_gas: u64,
    pub total_txs: u64,
    pub wall_clock_ms: f64,
    pub execution_ms: f64,
    pub avg_total_ms: f64,
    pub avg_new_payload_ms: Option<f64>,
    pub avg_fcu_ms: Option<f64>,
    pub avg_mgas_per_s: f64,
    pub avg_tx_per_s: f64,
    pub p50_new_payload_ms: Option<f64>,
    pub p95_new_payload_ms: Option<f64>,
    pub p99_new_payload_ms: Option<f64>,
    pub p50_fcu_ms: Option<f64>,
    pub p95_fcu_ms: Option<f64>,
    pub p99_fcu_ms: Option<f64>,
    pub p50_total_ms: f64,
    pub p95_total_ms: f64,
    pub p99_total_ms: f64,
}

pub(crate) trait BenchmarkRow {
    fn gas_used(&self) -> u64;
    fn tx_count(&self) -> u64;
    fn total_ms(&self) -> f64;
    fn new_payload_ms(&self) -> Option<f64> {
        None
    }
    fn fcu_ms(&self) -> Option<f64> {
        None
    }
}

impl BenchmarkRow for CombinedLatencyRow {
    fn gas_used(&self) -> u64 {
        self.gas_used
    }

    fn tx_count(&self) -> u64 {
        self.tx_count
    }

    fn total_ms(&self) -> f64 {
        self.total_ms
    }

    fn new_payload_ms(&self) -> Option<f64> {
        Some(self.new_payload_ms)
    }

    fn fcu_ms(&self) -> Option<f64> {
        Some(self.fcu_ms)
    }
}

pub(crate) fn throughput_mgas_per_s(gas_used: u64, duration: Duration) -> f64 {
    let seconds = duration.as_secs_f64();
    if seconds == 0.0 {
        return 0.0;
    }
    gas_used as f64 / seconds / 1_000_000.0
}

pub(crate) fn throughput_tx_per_s(tx_count: u64, duration: Duration) -> f64 {
    let seconds = duration.as_secs_f64();
    if seconds == 0.0 {
        return 0.0;
    }
    tx_count as f64 / seconds
}

pub(crate) struct CsvWriter {
    writer: csv::Writer<std::fs::File>,
    path: PathBuf,
}

impl CsvWriter {
    pub(crate) fn new(path: &Path) -> eyre::Result<Self> {
        let writer = csv::Writer::from_path(path)
            .wrap_err_with(|| format!("failed to create {}", path.display()))?;
        Ok(Self {
            writer,
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn write_row<T: Serialize>(&mut self, row: &T) -> eyre::Result<()> {
        self.writer
            .serialize(row)
            .wrap_err_with(|| format!("failed to write row to {}", self.path.display()))
    }

    pub(crate) fn finish(mut self) -> eyre::Result<()> {
        self.writer
            .flush()
            .wrap_err_with(|| format!("failed to flush {}", self.path.display()))
    }
}

pub(crate) fn write_csv<T: Serialize>(path: &Path, rows: &[T]) -> eyre::Result<()> {
    let mut writer = CsvWriter::new(path)?;
    for row in rows {
        writer.write_row(row)?;
    }
    writer.finish()
}

pub(crate) fn build_summary<T: BenchmarkRow>(
    mode: &str,
    rows: &[T],
    wall_clock: Duration,
) -> eyre::Result<SummaryRow> {
    if rows.is_empty() {
        bail!("cannot build a summary for an empty benchmark result set");
    }

    let total_gas = rows.iter().map(BenchmarkRow::gas_used).sum::<u64>();
    let total_txs = rows.iter().map(BenchmarkRow::tx_count).sum::<u64>();
    let mut total_latencies = rows.iter().map(BenchmarkRow::total_ms).collect::<Vec<_>>();
    let mut new_payload_latencies = rows
        .iter()
        .filter_map(BenchmarkRow::new_payload_ms)
        .collect::<Vec<_>>();
    let mut fcu_latencies = rows
        .iter()
        .filter_map(BenchmarkRow::fcu_ms)
        .collect::<Vec<_>>();
    let execution_ms = total_latencies.iter().sum::<f64>();

    sort_f64(&mut total_latencies);
    sort_f64(&mut new_payload_latencies);
    sort_f64(&mut fcu_latencies);

    let pct_sorted_opt = |sorted: &[f64], q: f64| -> Option<f64> {
        (!sorted.is_empty()).then(|| percentile_sorted(sorted, q))
    };

    Ok(SummaryRow {
        mode: mode.to_owned(),
        samples: rows.len() as u64,
        total_gas,
        total_txs,
        wall_clock_ms: duration_to_ms(wall_clock),
        execution_ms,
        avg_total_ms: execution_ms / rows.len() as f64,
        avg_new_payload_ms: average(&new_payload_latencies),
        avg_fcu_ms: average(&fcu_latencies),
        avg_mgas_per_s: throughput_mgas_per_s(total_gas, wall_clock),
        avg_tx_per_s: throughput_tx_per_s(total_txs, wall_clock),
        p50_new_payload_ms: pct_sorted_opt(&new_payload_latencies, 0.50),
        p95_new_payload_ms: pct_sorted_opt(&new_payload_latencies, 0.95),
        p99_new_payload_ms: pct_sorted_opt(&new_payload_latencies, 0.99),
        p50_fcu_ms: pct_sorted_opt(&fcu_latencies, 0.50),
        p95_fcu_ms: pct_sorted_opt(&fcu_latencies, 0.95),
        p99_fcu_ms: pct_sorted_opt(&fcu_latencies, 0.99),
        p50_total_ms: percentile_sorted(&total_latencies, 0.50),
        p95_total_ms: percentile_sorted(&total_latencies, 0.95),
        p99_total_ms: percentile_sorted(&total_latencies, 0.99),
    })
}

fn average(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn sort_f64(values: &mut [f64]) {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
}

fn percentile_sorted(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = quantile.clamp(0.0, 1.0) * (sorted.len().saturating_sub(1)) as f64;
    let lower_index = rank.floor() as usize;
    let upper_index = rank.ceil() as usize;
    if lower_index == upper_index {
        sorted[lower_index]
    } else {
        sorted[lower_index]
            + (sorted[upper_index] - sorted[lower_index]) * (rank - lower_index as f64)
    }
}

#[cfg(test)]
fn percentile(mut values: Vec<f64>, quantile: f64) -> f64 {
    sort_f64(&mut values);
    percentile_sorted(&values, quantile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_interpolates_between_samples() {
        let actual = percentile(vec![10.0, 20.0, 30.0, 40.0], 0.75);
        assert_eq!(actual, 32.5);
    }

    #[test]
    fn summary_uses_wall_clock_for_average_throughput() {
        let row = CombinedLatencyRow {
            block_number: 1,
            block_hash: "0x01".to_string(),
            tx_count: 2,
            gas_used: 2_000_000,
            new_payload_ms: 100.0,
            fcu_ms: 50.0,
            total_ms: 150.0,
            elapsed_ms: 150.0,
            mgas_per_s: 0.0,
            tx_per_s: 0.0,
            cumulative_mgas_per_s: 0.0,
            cumulative_tx_per_s: 0.0,
        };
        let summary = build_summary("new-payload-fcu", &[row], Duration::from_millis(200)).unwrap();
        assert_eq!(summary.execution_ms, 150.0);
        assert_eq!(summary.avg_total_ms, 150.0);
        assert_eq!(summary.avg_new_payload_ms, Some(100.0));
        assert_eq!(summary.avg_fcu_ms, Some(50.0));
        assert_eq!(summary.avg_mgas_per_s, 10.0);
    }

    #[test]
    fn summary_includes_component_latency_percentiles() {
        let rows = [
            CombinedLatencyRow {
                block_number: 1,
                block_hash: "0x01".to_string(),
                tx_count: 1,
                gas_used: 1_000_000,
                new_payload_ms: 10.0,
                fcu_ms: 1.0,
                total_ms: 11.0,
                elapsed_ms: 11.0,
                mgas_per_s: 0.0,
                tx_per_s: 0.0,
                cumulative_mgas_per_s: 0.0,
                cumulative_tx_per_s: 0.0,
            },
            CombinedLatencyRow {
                block_number: 2,
                block_hash: "0x02".to_string(),
                tx_count: 1,
                gas_used: 1_000_000,
                new_payload_ms: 20.0,
                fcu_ms: 2.0,
                total_ms: 22.0,
                elapsed_ms: 33.0,
                mgas_per_s: 0.0,
                tx_per_s: 0.0,
                cumulative_mgas_per_s: 0.0,
                cumulative_tx_per_s: 0.0,
            },
        ];

        let summary = build_summary("new-payload-fcu", &rows, Duration::from_millis(33)).unwrap();
        assert_eq!(summary.p50_new_payload_ms, Some(15.0));
        assert_eq!(summary.p95_new_payload_ms, Some(19.5));
        assert_eq!(summary.p50_fcu_ms, Some(1.5));
        assert_eq!(summary.p95_fcu_ms, Some(1.95));
    }
}
