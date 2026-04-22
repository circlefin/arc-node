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

//! ARC-backed statetest runner and harness.
//!
//! This module owns both per-suite execution and file-level orchestration,
//! closer to upstream `revme`, while preserving ARC execution semantics and
//! the structured consume-direct output contract.
//!
//! Important execution-mode note:
//! - fixture fork names still choose the REVM `cfg.spec`
//! - the executor chain spec is always ARC `LOCAL_DEV`
//! - this runner therefore measures ARC localdev behavior under Ethereum
//!   fixture inputs, not pure Ethereum fork-isolated execution

use std::{
    collections::{BTreeSet, HashMap},
    io::stderr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{address, Address, Bytes, B256};
use arc_evm::ArcEvmFactory;
use reth_evm::{Evm, EvmEnv, EvmFactory};
use revm::{context::CfgEnv, database::State};
use revm_inspector::{inspectors::TracerEip3155, InspectCommitEvm};
use revm_primitives::hardfork::SpecId;
use revm_statetest_types::{SpecName, Test, TestSuite, TestUnit};

use crate::adapter::{
    build_default_arc_chain_spec, build_evm_env, build_evm_factory, extract_chain_ids,
    is_supported_spec, resolve_chain_id,
};
use crate::error::{EvmSpecsTestError, TestErrorKind};
use crate::exception_match::{
    exception_matches, tx_env_actual_exception, tx_env_exception_matches,
};
use crate::fixture_sanitizer::strip_unsupported_fields;
use crate::result::{JsonOutcome, TestResult, TestSummary};
use crate::roots::{
    compute_state_root_from_fixture_accounts, compute_test_roots, state_merkle_trie_root,
    TestValidationResult,
};

const ARC_NATIVE_COIN_AUTHORITY: Address = address!("1800000000000000000000000000000000000000");
const ARC_NATIVE_COIN_CONTROL: Address = address!("1800000000000000000000000000000000000001");

const SIGNAL_ARC_NATIVE_COIN_AUTHORITY_LOG_PRESENT: &str = "arc_native_coin_authority_log_present";
const SIGNAL_ARC_NATIVE_COIN_CONTROL_STATE_TOUCHED: &str = "arc_native_coin_control_state_touched";
const SIGNAL_ARC_SYSTEM_ACCOUNT_TOUCHED: &str = "arc_system_account_touched";
const SIGNAL_PRECOMPILE_ADDRESS_TOUCHED: &str = "precompile_address_touched";
const SIGNAL_COINBASE_TOUCHED: &str = "coinbase_touched";
const SIGNAL_FIXTURE_ORACLE_ROOT_MATCHES_FIXTURE_HASH: &str =
    "fixture_oracle_root_matches_fixture_hash";

#[derive(Default)]
struct FileRunOutput {
    results: Vec<TestResult>,
    summary: TestSummary,
    fatal_file_errors: usize,
}

struct RunnerConfig {
    filter_name: Option<String>,
    trace: bool,
    json_outcome: bool,
}

#[derive(Clone)]
struct RunnerState {
    completed_files: Arc<AtomicUsize>,
    queue: Arc<Mutex<(usize, Vec<PathBuf>)>>,
    total_files: usize,
}

impl RunnerState {
    fn new(files: Vec<PathBuf>) -> Self {
        let total_files = files.len();
        Self {
            completed_files: Arc::new(AtomicUsize::new(0)),
            queue: Arc::new(Mutex::new((0, files))),
            total_files,
        }
    }

    fn next_file(&self) -> Result<Option<(usize, PathBuf)>, EvmSpecsTestError> {
        let (next_idx, files) = &mut *self
            .queue
            .lock()
            .map_err(|_| EvmSpecsTestError::RunnerQueuePoisoned)?;
        let index = *next_idx;
        let Some(path) = files.get(index).cloned() else {
            return Ok(None);
        };
        *next_idx = index.checked_add(1).expect("runner queue index overflow");
        Ok(Some((index, path)))
    }
}

struct TestExecutionContext<'a> {
    factory: &'a ArcEvmFactory,
    cache_state: &'a revm_database::states::CacheState,
    evm_env: &'a EvmEnv,
    unit: &'a TestUnit,
    test: &'a Test,
    test_id: &'a str,
}

struct DebugContext<'a> {
    factory: &'a ArcEvmFactory,
    cache_state: &'a revm_database::states::CacheState,
    evm_env: &'a EvmEnv,
    unit: &'a TestUnit,
    test: &'a Test,
    test_id: &'a str,
    error: &'a EvmSpecsTestError,
}

struct TestExecutionReport {
    error: Option<EvmSpecsTestError>,
    json_outcome: Option<JsonOutcome>,
}

struct LoadedFixtureFile {
    suite: TestSuite,
    chain_id_map: HashMap<String, u64>,
}

struct TestUnitExecution<'a, 'b> {
    name: &'a str,
    unit: &'a TestUnit,
    factory: &'a ArcEvmFactory,
    chain_id_map: &'a HashMap<String, u64>,
    filter_name: Option<&'a str>,
    trace: bool,
    json_outcome: bool,
    summary: &'b mut TestSummary,
    results: &'b mut Vec<TestResult>,
}

pub fn run(
    path: PathBuf,
    filter_name: Option<String>,
    strict_exit: bool,
    trace: bool,
    json_outcome: bool,
) -> Result<crate::result::RunStatus, EvmSpecsTestError> {
    use crate::result::RunStatus;

    let json_files = find_json_files(&path)?;
    if json_files.is_empty() {
        return Err(EvmSpecsTestError::NoJsonFiles {
            path: path.display().to_string(),
        });
    }

    let state = RunnerState::new(json_files);
    let config = Arc::new(RunnerConfig {
        filter_name,
        trace,
        json_outcome,
    });
    let num_threads = determine_thread_count(state.total_files, trace);

    let mut handles = Vec::with_capacity(num_threads);
    for worker_id in 0..num_threads {
        let state = state.clone();
        let config = Arc::clone(&config);
        let thread = std::thread::Builder::new()
            .name(format!("arc-evm-specs-tests-runner-{worker_id}"))
            .spawn(move || run_file_worker(state, config))
            .map_err(EvmSpecsTestError::WorkerSpawn)?;
        handles.push(thread);
    }

    let mut indexed_outputs = Vec::new();
    for handle in handles {
        let output = handle
            .join()
            .map_err(|_| EvmSpecsTestError::WorkerPanic)??;
        indexed_outputs.extend(output);
    }
    indexed_outputs.sort_by_key(|(index, _)| *index);

    let mut all_results = Vec::new();
    let mut total_summary = TestSummary::default();
    let mut fatal_file_errors = 0usize;

    for (_, output) in indexed_outputs {
        all_results.extend(output.results);
        fatal_file_errors = fatal_file_errors
            .checked_add(output.fatal_file_errors)
            .expect("fatal_file_errors overflow");
        total_summary.add_files_processed(output.summary.files_processed);
        total_summary.add_tests_total(output.summary.tests_total);
        total_summary.add_tests_passed(output.summary.tests_passed);
        total_summary.add_tests_failed(output.summary.tests_failed);
        total_summary.add_tests_skipped_by_spec(output.summary.tests_skipped_by_spec);
    }

    let json_output =
        serde_json::to_string_pretty(&all_results).expect("Failed to serialize results");
    println!("{json_output}");
    eprintln!("{total_summary}");

    if fatal_file_errors > 0 {
        return Ok(RunStatus::FatalFileErrors);
    }
    if strict_exit && total_summary.tests_failed > 0 {
        return Ok(RunStatus::TestsFailed);
    }

    Ok(RunStatus::Success)
}

/// Execute all tests in a deserialized TestSuite.
///
/// `chain_id_map` is built from raw JSON via `extract_chain_ids()` before
/// calling this function (two-pass parse pattern).
///
/// Returns a vec of TestResult (one per test variant) and updates the summary.
pub fn execute_test_suite(
    suite: &TestSuite,
    factory: &ArcEvmFactory,
    chain_id_map: &HashMap<String, u64>,
    filter_name: Option<&str>,
    trace: bool,
    json_outcome: bool,
) -> (Vec<TestResult>, TestSummary) {
    let mut summary = TestSummary::default();
    let mut results = Vec::new();

    for (name, unit) in &suite.0 {
        execute_test_unit(TestUnitExecution {
            name,
            unit,
            factory,
            chain_id_map,
            filter_name,
            trace,
            json_outcome,
            summary: &mut summary,
            results: &mut results,
        });
    }

    (results, summary)
}

fn run_file_worker(
    state: RunnerState,
    config: Arc<RunnerConfig>,
) -> Result<Vec<(usize, FileRunOutput)>, EvmSpecsTestError> {
    let mut outputs = Vec::new();

    while let Some((index, file)) = state.next_file()? {
        let output = process_fixture_file(
            &file,
            config.filter_name.as_deref(),
            config.trace,
            config.json_outcome,
            &state.completed_files,
            state.total_files,
        );
        outputs.push((index, output));
    }

    Ok(outputs)
}

fn process_fixture_file(
    file: &Path,
    filter_name: Option<&str>,
    trace: bool,
    json_outcome: bool,
    completed_files: &AtomicUsize,
    total_files: usize,
) -> FileRunOutput {
    let loaded_fixture = match load_fixture_file(file) {
        Ok(loaded_fixture) => loaded_fixture,
        Err(error) => {
            report_progress(completed_files, total_files);
            return file_error_result(file, error);
        }
    };

    let chain_spec = build_default_arc_chain_spec();
    let factory = build_evm_factory(chain_spec);
    let (results, mut summary) = execute_test_suite(
        &loaded_fixture.suite,
        &factory,
        &loaded_fixture.chain_id_map,
        filter_name,
        trace,
        json_outcome,
    );
    summary.files_processed = 1;

    report_progress(completed_files, total_files);

    FileRunOutput {
        results,
        summary,
        fatal_file_errors: 0,
    }
}

fn load_fixture_file(file: &Path) -> Result<LoadedFixtureFile, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("read error: {e}"))?;
    let raw_json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        EvmSpecsTestError::JsonParse {
            path: file.display().to_string(),
            source,
        }
        .to_string()
    })?;

    if !looks_like_statetest_fixture(&raw_json) {
        return Err(format!(
            "unsupported or malformed statetest fixture shape in {}",
            file.display()
        ));
    }

    let chain_id_map = extract_chain_ids(&raw_json).map_err(|e| e.to_string())?;
    let mut sanitized_json = raw_json;
    strip_unsupported_fields(&mut sanitized_json);
    let suite = serde_json::from_value(sanitized_json).map_err(|source| {
        EvmSpecsTestError::JsonParse {
            path: file.display().to_string(),
            source,
        }
        .to_string()
    })?;

    Ok(LoadedFixtureFile {
        suite,
        chain_id_map,
    })
}

fn execute_test_unit(exec: TestUnitExecution<'_, '_>) {
    let TestUnitExecution {
        name,
        unit,
        factory,
        chain_id_map,
        filter_name,
        trace,
        json_outcome,
        summary,
        results,
    } = exec;
    let cache_state = unit.state();

    let chain_id =
        match resolve_test_unit_chain_id(name, unit, chain_id_map, filter_name, summary, results) {
            Some(chain_id) => chain_id,
            None => return,
        };

    for (spec_name, tests) in &unit.post {
        execute_spec_tests(
            name,
            unit,
            factory,
            &cache_state,
            chain_id,
            spec_name,
            tests,
            filter_name,
            trace,
            json_outcome,
            summary,
            results,
        );
    }
}

fn resolve_test_unit_chain_id(
    name: &str,
    unit: &TestUnit,
    chain_id_map: &HashMap<String, u64>,
    filter_name: Option<&str>,
    summary: &mut TestSummary,
    results: &mut Vec<TestResult>,
) -> Option<u64> {
    match resolve_chain_id(name, chain_id_map, unit) {
        Ok(chain_id) => Some(chain_id),
        Err(error) => {
            let rendered_error = error.to_string();
            for (spec_name, tests) in &unit.post {
                push_failures_for_spec_variants(
                    name,
                    spec_name,
                    tests,
                    filter_name,
                    &rendered_error,
                    summary,
                    results,
                );
            }
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_spec_tests(
    name: &str,
    unit: &TestUnit,
    factory: &ArcEvmFactory,
    cache_state: &revm_database::states::CacheState,
    chain_id: u64,
    spec_name: &SpecName,
    tests: &[Test],
    filter_name: Option<&str>,
    trace: bool,
    json_outcome: bool,
    summary: &mut TestSummary,
    results: &mut Vec<TestResult>,
) {
    if !is_supported_spec(spec_name) {
        record_skipped_spec_tests(name, spec_name, tests, filter_name, summary);
        return;
    }

    let evm_env = match build_spec_evm_env(unit, spec_name, chain_id) {
        Ok(evm_env) => evm_env,
        Err(error) => {
            push_failures_for_spec_variants(
                name,
                spec_name,
                tests,
                filter_name,
                &error,
                summary,
                results,
            );
            return;
        }
    };

    for test in tests {
        execute_spec_test(TestCaseExecution {
            name,
            unit,
            factory,
            cache_state,
            evm_env: &evm_env,
            spec_name,
            test,
            filter_name,
            trace,
            json_outcome,
            summary,
            results,
        });
    }
}

fn build_spec_evm_env(
    unit: &TestUnit,
    spec_name: &SpecName,
    chain_id: u64,
) -> Result<EvmEnv, String> {
    let mut evm_env = build_evm_env(unit, spec_name, chain_id).map_err(|e| e.to_string())?;
    configure_blob_limits(&mut evm_env.cfg_env);
    Ok(evm_env)
}

struct TestCaseExecution<'a, 'b> {
    name: &'a str,
    unit: &'a TestUnit,
    factory: &'a ArcEvmFactory,
    cache_state: &'a revm_database::states::CacheState,
    evm_env: &'a EvmEnv,
    spec_name: &'a SpecName,
    test: &'a Test,
    filter_name: Option<&'a str>,
    trace: bool,
    json_outcome: bool,
    summary: &'b mut TestSummary,
    results: &'b mut Vec<TestResult>,
}

fn execute_spec_test(exec: TestCaseExecution<'_, '_>) {
    let test_id = format_test_id(exec.name, exec.spec_name, &exec.test.indexes);
    if let Some(filter) = exec.filter_name
        && !matches_filter(filter, &test_id, exec.name, exec.spec_name)
    {
        return;
    }

    let result_name = output_name_for_filter(exec.filter_name, &test_id, exec.name, exec.spec_name);
    exec.summary.add_tests_total(1);

    let ctx = TestExecutionContext {
        factory: exec.factory,
        cache_state: exec.cache_state,
        evm_env: exec.evm_env,
        unit: exec.unit,
        test: exec.test,
        test_id: &test_id,
    };

    let report = execute_single_test(ctx, exec.json_outcome);
    push_test_execution_result(exec, test_id, result_name, report);
}

fn push_test_execution_result(
    exec: TestCaseExecution<'_, '_>,
    test_id: String,
    result_name: String,
    report: TestExecutionReport,
) {
    match report {
        TestExecutionReport {
            error: None,
            json_outcome,
        } => {
            exec.summary.add_tests_passed(1);
            if let Some(outcome) = json_outcome.as_ref() {
                emit_json_outcome_line(outcome, true);
            }
            let result = TestResult::passed(result_name).with_variant_id(test_id);
            exec.results.push(match json_outcome {
                Some(outcome) => result.with_json_outcome(outcome),
                None => result,
            });
        }
        TestExecutionReport {
            error: Some(error),
            json_outcome,
        } => {
            if exec.trace {
                debug_failed_test(DebugContext {
                    factory: exec.factory,
                    cache_state: exec.cache_state,
                    evm_env: exec.evm_env,
                    unit: exec.unit,
                    test: exec.test,
                    test_id: &test_id,
                    error: &error,
                });
            }
            exec.summary.add_tests_failed(1);
            if let Some(outcome) = json_outcome.as_ref() {
                emit_json_outcome_line(outcome, false);
            }
            let result =
                TestResult::failed(result_name, error.to_string()).with_variant_id(test_id);
            exec.results.push(match json_outcome {
                Some(outcome) => result.with_json_outcome(outcome),
                None => result,
            });
        }
    }
}

fn emit_json_outcome_line(outcome: &JsonOutcome, pass: bool) {
    eprintln!("{}", build_json_outcome_report(outcome, pass));
}

fn build_json_outcome_report(outcome: &JsonOutcome, pass: bool) -> serde_json::Value {
    serde_json::json!({
        "stateRoot": outcome.state_root,
        "logsRoot": outcome.logs_root,
        "output": outcome.output,
        "gasUsed": outcome.gas_used,
        "pass": pass,
        "errorMsg": outcome.error_msg,
        "evmResult": outcome.evm_result,
        "postLogsHash": outcome.post_logs_hash,
        "fork": outcome.fork,
        "test": outcome.test,
        // Keep the legacy aliases alongside the readable names so existing
        // revm-style parsers do not silently break when consume-direct starts
        // persisting `per_test_outcomes.jsonl`.
        "d": outcome.data_index,
        "g": outcome.gas_index,
        "v": outcome.value_index,
        "data_index": outcome.data_index,
        "gas_index": outcome.gas_index,
        "value_index": outcome.value_index,
    })
}

fn record_skipped_spec_tests(
    name: &str,
    spec_name: &SpecName,
    tests: &[Test],
    filter_name: Option<&str>,
    summary: &mut TestSummary,
) {
    if let Some(filter) = filter_name {
        for test in tests {
            let test_id = format_test_id(name, spec_name, &test.indexes);
            if matches_filter(filter, &test_id, name, spec_name) {
                summary.add_tests_skipped_by_spec(1);
            }
        }
        return;
    }
    summary.add_tests_skipped_by_spec(tests.len());
}

fn push_failures_for_spec_variants(
    name: &str,
    spec_name: &SpecName,
    tests: &[Test],
    filter_name: Option<&str>,
    error: &str,
    summary: &mut TestSummary,
    results: &mut Vec<TestResult>,
) {
    for test in tests {
        let test_id = format_test_id(name, spec_name, &test.indexes);
        if let Some(filter) = filter_name
            && !matches_filter(filter, &test_id, name, spec_name)
        {
            continue;
        }
        let result_name = output_name_for_filter(filter_name, &test_id, name, spec_name);
        summary.add_tests_total(1);
        summary.add_tests_failed(1);
        results.push(TestResult::failed(result_name, error.to_string()).with_variant_id(test_id));
    }
}

fn configure_blob_limits(cfg: &mut CfgEnv) {
    if cfg.spec.is_enabled_in(SpecId::OSAKA) {
        cfg.set_max_blobs_per_tx(6);
    } else if cfg.spec.is_enabled_in(SpecId::PRAGUE) {
        cfg.set_max_blobs_per_tx(9);
    } else {
        cfg.set_max_blobs_per_tx(6);
    }
}

/// Execute a single test variant and validate results.
fn execute_single_test(ctx: TestExecutionContext<'_>, json_outcome: bool) -> TestExecutionReport {
    let tx = match ctx.test.tx_env(ctx.unit) {
        Ok(tx) => tx,
        Err(err) => match handle_tx_env_error(&ctx, &err.to_string()) {
            Ok(()) => {
                return TestExecutionReport {
                    error: None,
                    json_outcome: None,
                };
            }
            Err(error) => {
                return TestExecutionReport {
                    error: Some(error),
                    json_outcome: None,
                };
            }
        },
    };

    let state = State::builder()
        .with_cached_prestate(ctx.cache_state.clone())
        .with_bundle_update()
        .build();

    let mut evm = ctx.factory.create_evm(state, ctx.evm_env.clone());
    let exec_result = evm.transact_commit(tx);
    let db = &*evm.db_mut();

    let validation = compute_test_roots(&exec_result, db);
    let error = evaluate_evm_execution(&ctx, ctx.unit.out.as_ref(), &exec_result, db, &validation);
    let json_outcome = json_outcome.then(|| {
        build_json_outcome(
            &ctx,
            &exec_result,
            &validation,
            error.as_ref().map(std::string::ToString::to_string),
        )
    });

    TestExecutionReport {
        error,
        json_outcome,
    }
}

fn handle_tx_env_error(ctx: &TestExecutionContext<'_>, err: &str) -> Result<(), EvmSpecsTestError> {
    let actual_exception = tx_env_actual_exception(err).unwrap_or(err);

    if let Some(expected) = &ctx.test.expect_exception {
        if tx_env_exception_matches(expected, actual_exception) {
            return Ok(());
        }

        return Err(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "WRONG_EXCEPTION",
                format!("expected_exception={expected}, got_exception={actual_exception}"),
            ),
        });
    }

    Err(EvmSpecsTestError::TestFailure {
        name: ctx.test_id.to_string(),
        kind: TestErrorKind::evm(
            "HARNESS_PRECONDITION",
            "TX_ENV_BUILD_FAILED",
            err.to_string(),
        ),
    })
}

fn validate_expected_exception(
    ctx: &TestExecutionContext<'_>,
    exec_result: &Result<
        revm::context::result::ExecutionResult<revm::context::result::HaltReason>,
        revm::context::result::EVMError<
            revm::database::bal::EvmDatabaseError<std::convert::Infallible>,
            revm::context::result::InvalidTransaction,
        >,
    >,
) -> Result<bool, EvmSpecsTestError> {
    match (&ctx.test.expect_exception, exec_result) {
        (None, Err(e)) => Err(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "UNEXPECTED_EXCEPTION",
                format!("expected_exception=None, got_exception={}", e),
            ),
        }),
        (Some(expected), Ok(_)) => Err(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "UNEXPECTED_SUCCESS",
                format!("expected_exception={expected}, got_exception=None"),
            ),
        }),
        (Some(expected), Err(actual)) => {
            let actual = actual.to_string();
            if exception_matches(expected, &actual) {
                Ok(true)
            } else {
                Err(EvmSpecsTestError::TestFailure {
                    name: ctx.test_id.to_string(),
                    kind: TestErrorKind::evm(
                        "EXECUTION_MISMATCH",
                        "WRONG_EXCEPTION",
                        format!("expected_exception={expected}, got_exception={actual}"),
                    ),
                })
            }
        }
        (None, Ok(_)) => Ok(false),
    }
}

fn validate_output(
    ctx: &TestExecutionContext<'_>,
    expected_output: Option<&Bytes>,
    actual_result: &revm::context::result::ExecutionResult<revm::context::result::HaltReason>,
) -> Result<(), EvmSpecsTestError> {
    if let Some((expected, actual)) = expected_output.zip(actual_result.output())
        && expected != actual
    {
        return Err(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "UNEXPECTED_OUTPUT",
                format!("expected_output={expected:?}, got_output={actual:?}"),
            ),
        });
    }

    Ok(())
}

fn evaluate_evm_execution(
    ctx: &TestExecutionContext<'_>,
    expected_output: Option<&Bytes>,
    exec_result: &Result<
        revm::context::result::ExecutionResult<revm::context::result::HaltReason>,
        revm::context::result::EVMError<
            revm::database::bal::EvmDatabaseError<std::convert::Infallible>,
            revm::context::result::InvalidTransaction,
        >,
    >,
    db: &State<revm::database::EmptyDB>,
    validation: &TestValidationResult,
) -> Option<EvmSpecsTestError> {
    let logs = exec_result
        .as_ref()
        .map(|result| result.logs())
        .unwrap_or_default();

    match validate_expected_exception(ctx, exec_result) {
        Ok(true) => return None,
        Ok(false) => {}
        Err(error) => return Some(error),
    }

    if let Ok(result) = exec_result
        && let Err(error) = validate_output(ctx, expected_output, result)
    {
        return Some(error);
    }

    if validation.logs_root != ctx.test.logs {
        let logs_preview = summarize_logs(logs);
        let signals = summarize_arc_signals(logs, db, None, ctx.unit.env.current_coinbase);
        return Some(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "LOGS_HASH_MISMATCH",
                format!(
                    "expected={}, got={}; logs_count={}, {}; signals={signals}",
                    ctx.test.logs,
                    validation.logs_root,
                    logs.len(),
                    logs_preview
                ),
            ),
        });
    }

    if validation.state_root != ctx.test.hash {
        let diagnostic = build_state_root_diagnostic(ctx, db, logs);
        return Some(EvmSpecsTestError::TestFailure {
            name: ctx.test_id.to_string(),
            kind: TestErrorKind::evm(
                "EXECUTION_MISMATCH",
                "STATE_ROOT_MISMATCH",
                format!(
                    "expected={}, got={}; diagnostic: {diagnostic}",
                    ctx.test.hash, validation.state_root
                ),
            ),
        });
    }

    None
}

fn build_state_root_diagnostic(
    ctx: &TestExecutionContext<'_>,
    db: &State<revm::database::EmptyDB>,
    logs: &[revm_primitives::Log],
) -> String {
    let coinbase = ctx.unit.env.current_coinbase;
    let coinbase_delta = db
        .cache
        .accounts
        .get(&coinbase)
        .and_then(|account| account.account.as_ref())
        .map(|account| {
            format!(
                "nonce={},balance={}",
                account.info.nonce, account.info.balance
            )
        })
        .unwrap_or_else(|| "missing".to_string());
    let touched_accounts = summarize_touched_accounts(db);
    let actual_trie_accounts: BTreeSet<_> = db
        .cache
        .trie_account()
        .into_iter()
        .map(|(address, _)| address)
        .collect();
    let filtered_arc_system_root = state_merkle_trie_root(
        db.cache
            .trie_account()
            .into_iter()
            .filter(|(address, _)| *address != ARC_NATIVE_COIN_CONTROL),
    );
    let filtered_arc_system_and_coinbase_root = state_merkle_trie_root(
        db.cache
            .trie_account()
            .into_iter()
            .filter(|(address, _)| *address != ARC_NATIVE_COIN_CONTROL && *address != coinbase),
    );

    if ctx.test.post_state.is_empty() {
        let signals = summarize_arc_signals(logs, db, None, coinbase);
        return format!(
            "fixture postState unavailable; actual_trie_accounts={}, filtered_arc_system_root={filtered_arc_system_root}, filtered_arc_system_and_coinbase_root={filtered_arc_system_and_coinbase_root}, fixture_hash={}, coinbase={}, coinbase_delta={coinbase_delta}, touched_accounts={touched_accounts}, signals={signals}",
            actual_trie_accounts.len(),
            ctx.test.hash,
            coinbase
        );
    }

    let oracle_root = compute_state_root_from_fixture_accounts(&ctx.test.post_state);
    let expected_trie_accounts: BTreeSet<_> = ctx.test.post_state.keys().copied().collect();
    let extra_actual_accounts = actual_trie_accounts
        .difference(&expected_trie_accounts)
        .take(6)
        .map(|address| address.to_string())
        .collect::<Vec<_>>()
        .join("|");
    let missing_expected_accounts = expected_trie_accounts
        .difference(&actual_trie_accounts)
        .take(6)
        .map(|address| address.to_string())
        .collect::<Vec<_>>()
        .join("|");
    let signals = summarize_arc_signals(logs, db, Some((oracle_root, ctx.test.hash)), coinbase);

    format!(
        "fixture_postState_root={oracle_root}, actual_trie_accounts={}, expected_trie_accounts={}, extra_actual_accounts={}, missing_expected_accounts={}, filtered_arc_system_root={filtered_arc_system_root}, filtered_arc_system_and_coinbase_root={filtered_arc_system_and_coinbase_root}, fixture_hash={}, coinbase={}, coinbase_delta={coinbase_delta}, touched_accounts={touched_accounts}, signals={signals}",
        actual_trie_accounts.len(),
        expected_trie_accounts.len(),
        if extra_actual_accounts.is_empty() { "none" } else { &extra_actual_accounts },
        if missing_expected_accounts.is_empty() { "none" } else { &missing_expected_accounts },
        ctx.test.hash,
        coinbase
    )
}

fn build_json_outcome(
    ctx: &TestExecutionContext<'_>,
    exec_result: &Result<
        revm::context::result::ExecutionResult<revm::context::result::HaltReason>,
        revm::context::result::EVMError<
            revm::database::bal::EvmDatabaseError<std::convert::Infallible>,
            revm::context::result::InvalidTransaction,
        >,
    >,
    validation: &TestValidationResult,
    error: Option<String>,
) -> JsonOutcome {
    JsonOutcome {
        state_root: validation.state_root,
        logs_root: validation.logs_root,
        output: exec_result
            .as_ref()
            .ok()
            .and_then(|result| result.output().cloned())
            .unwrap_or_default(),
        gas_used: exec_result
            .as_ref()
            .ok()
            .map(|result| result.gas_used())
            .unwrap_or_default(),
        error_msg: error.unwrap_or_default(),
        evm_result: format_evm_result(exec_result),
        post_logs_hash: validation.logs_root,
        fork: format!("{:?}", ctx.evm_env.cfg_env.spec),
        test: ctx.test_id.to_string(),
        data_index: ctx.test.indexes.data,
        gas_index: ctx.test.indexes.gas,
        value_index: ctx.test.indexes.value,
    }
}

fn format_evm_result(
    exec_result: &Result<
        revm::context::result::ExecutionResult<revm::context::result::HaltReason>,
        revm::context::result::EVMError<
            revm::database::bal::EvmDatabaseError<std::convert::Infallible>,
            revm::context::result::InvalidTransaction,
        >,
    >,
) -> String {
    match exec_result {
        Ok(result) => match result {
            revm::context::result::ExecutionResult::Success { reason, .. } => {
                format!("Success: {reason:?}")
            }
            revm::context::result::ExecutionResult::Revert { .. } => "Revert".to_string(),
            revm::context::result::ExecutionResult::Halt { reason, .. } => {
                format!("Halt: {reason:?}")
            }
        },
        Err(error) => error.to_string(),
    }
}

/// Format the stable identifier for a concrete fixture variant.
///
/// This ID ties together the suite-level pytest HTML report and the
/// variant-level `per_test_outcomes.jsonl` artifact emitted during
/// `consume direct`, so keep it stable unless the downstream reporting
/// contract is updated in lockstep.
fn format_test_id(
    name: &str,
    spec: &SpecName,
    indexes: &revm_statetest_types::TxPartIndices,
) -> String {
    // Keep the historical `d{}_g{}_v{}` suffix inherited from the upstream
    // `revm` statetest runner shape (`bins/revme/src/cmd/statetest/runner.rs`).
    format!(
        "{name}/{spec:?}/d{}_g{}_v{}",
        indexes.data, indexes.gas, indexes.value
    )
}

fn summarize_logs(logs: &[revm_primitives::Log]) -> String {
    if logs.is_empty() {
        return "first_log=none".to_string();
    }

    let first = &logs[0];
    let topics_len = first.data.topics().len();
    let data_len = first.data.data.len();
    format!(
        "first_log=address={},topics={},data_len={}",
        first.address, topics_len, data_len
    )
}

fn summarize_arc_signals(
    logs: &[revm_primitives::Log],
    db: &State<revm::database::EmptyDB>,
    fixture_roots: Option<(B256, B256)>,
    coinbase: Address,
) -> String {
    let mut signals = Vec::new();

    if logs
        .iter()
        .any(|log| log.address == ARC_NATIVE_COIN_AUTHORITY)
    {
        signals.push(SIGNAL_ARC_NATIVE_COIN_AUTHORITY_LOG_PRESENT.to_string());
    }

    if db.cache.accounts.contains_key(&ARC_NATIVE_COIN_CONTROL) {
        signals.push(format!(
            "{SIGNAL_ARC_NATIVE_COIN_CONTROL_STATE_TOUCHED}={ARC_NATIVE_COIN_CONTROL}"
        ));
        signals.push(format!(
            "{SIGNAL_ARC_SYSTEM_ACCOUNT_TOUCHED}={ARC_NATIVE_COIN_CONTROL}"
        ));
    }

    if db.cache.accounts.contains_key(&coinbase) {
        signals.push(format!("{SIGNAL_COINBASE_TOUCHED}={coinbase}"));
    }

    if let Some(precompile_address) = first_touched_precompile_address(db) {
        signals.push(format!(
            "{SIGNAL_PRECOMPILE_ADDRESS_TOUCHED}={precompile_address}"
        ));
    }

    if let Some((oracle_root, fixture_hash)) = fixture_roots
        && oracle_root == fixture_hash
    {
        signals.push(SIGNAL_FIXTURE_ORACLE_ROOT_MATCHES_FIXTURE_HASH.to_string());
    }

    if signals.is_empty() {
        "none".to_string()
    } else {
        signals.join("|")
    }
}

fn summarize_touched_accounts(db: &State<revm::database::EmptyDB>) -> String {
    let mut entries: Vec<_> = db.cache.accounts.iter().collect();
    entries.sort_by_key(|(address, _)| *address);

    let preview = entries
        .into_iter()
        .take(4)
        .filter_map(|(address, account)| account.account.as_ref().map(|plain| (address, plain)))
        .map(|(address, account)| {
            format!(
                "{address}:nonce={},balance={},code_hash={},storage_slots={},selfdestructed={}",
                account.info.nonce,
                account.info.balance,
                account.info.code_hash,
                account.storage.len(),
                false
            )
        })
        .collect::<Vec<_>>()
        .join("|");

    if preview.is_empty() {
        "none".to_string()
    } else {
        preview
    }
}

fn first_touched_precompile_address(db: &State<revm::database::EmptyDB>) -> Option<Address> {
    let mut addresses: Vec<_> = db.cache.accounts.keys().copied().collect();
    addresses.sort();
    addresses
        .into_iter()
        .find(|address| is_precompile_address(*address))
}

fn is_precompile_address(address: Address) -> bool {
    let bytes = address.as_slice();
    if !bytes[..18].iter().all(|byte| *byte == 0) {
        return false;
    }

    let index = u16::from_be_bytes([bytes[18], bytes[19]]);
    matches!(index, 0x0001..=0x0011 | 0x0100)
}

fn matches_filter(filter: &str, test_id: &str, fixture_name: &str, _spec: &SpecName) -> bool {
    let filter_norm = normalize_fixture_name(filter);
    let fixture_norm = normalize_fixture_name(fixture_name);
    filter == test_id
        || filter == fixture_name
        || filter_norm == fixture_norm
        || is_consume_state_filter_for_fixture(filter, fixture_name)
}

fn output_name_for_filter(
    filter_name: Option<&str>,
    test_id: &str,
    fixture_name: &str,
    _spec: &SpecName,
) -> String {
    match filter_name {
        Some(filter) if filter == fixture_name => test_id.to_string(),
        Some(filter) if normalize_fixture_name(filter) == normalize_fixture_name(fixture_name) => {
            test_id.to_string()
        }
        Some(filter) if is_consume_state_filter_for_fixture(filter, fixture_name) => {
            filter.to_string()
        }
        _ => fixture_name.to_string(),
    }
}

fn is_consume_state_filter_for_fixture(filter: &str, fixture_name: &str) -> bool {
    let Some(suffix) = filter.strip_prefix(fixture_name) else {
        return false;
    };
    suffix.starts_with("[fork_") && suffix.ends_with("-state_test]")
}

fn normalize_fixture_name(name: &str) -> &str {
    name.strip_prefix("./").unwrap_or(name)
}

fn debug_failed_test(ctx: DebugContext<'_>) {
    eprintln!("\nTraces:");

    let tx = match ctx.test.tx_env(ctx.unit) {
        Ok(tx) => tx,
        Err(err) => {
            eprintln!("Unable to rebuild tx for trace rerun: {err}");
            eprintln!(
                "\nTest name: {:?} failed before trace rerun:\n{}",
                ctx.test_id, ctx.error
            );
            return;
        }
    };

    let state = State::builder()
        .with_cached_prestate(ctx.cache_state.clone())
        .with_bundle_update()
        .build();

    let tracer = TracerEip3155::buffered(stderr()).without_summary();
    let mut evm = ctx
        .factory
        .create_evm_with_inspector(state, ctx.evm_env.clone(), tracer);
    let exec_result = evm.inner.inspect_tx_commit(tx.clone());

    eprintln!("\nExecution result: {exec_result:#?}");
    eprintln!("\nExpected exception: {:?}", ctx.test.expect_exception);
    eprintln!("\nState before:\n{}", ctx.cache_state.pretty_print());
    eprintln!("\nState after:\n{}", evm.db_mut().cache.pretty_print());
    eprintln!("\nSpecification: {:?}", ctx.evm_env.cfg_env.spec);
    eprintln!("\nTx: {tx:#?}");
    eprintln!("Block: {:#?}", ctx.evm_env.block_env);
    eprintln!("Cfg: {:#?}", ctx.evm_env.cfg_env);
    eprintln!("\nTest name: {:?} failed:\n{}", ctx.test_id, ctx.error);
}

fn determine_thread_count(total_files: usize, trace: bool) -> usize {
    if trace {
        return 1;
    }
    std::thread::available_parallelism()
        .map(|count| count.get().min(total_files))
        .unwrap_or(1)
        .max(1)
}

fn find_json_files(path: &Path) -> Result<Vec<PathBuf>, EvmSpecsTestError> {
    let mut files = Vec::new();
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path.to_path_buf());
        }
    } else if path.is_dir() {
        collect_json_files_recursive(path, &mut files)?;
    }
    Ok(files)
}

fn collect_json_files_recursive(
    dir: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), EvmSpecsTestError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files_recursive(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    Ok(())
}

fn file_error_result(file: &Path, error: String) -> FileRunOutput {
    let summary = TestSummary {
        files_processed: 1,
        ..TestSummary::default()
    };

    FileRunOutput {
        results: vec![TestResult::failed(
            format!("__file_error__/{}", file.display()),
            error,
        )],
        summary,
        fatal_file_errors: 1,
    }
}

fn looks_like_statetest_fixture(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.values().any(|entry| {
        entry.is_object()
            && entry.get("env").is_some()
            && entry.get("post").is_some()
            && entry.get("pre").is_some()
    })
}

fn report_progress(completed_files: &AtomicUsize, total_files: usize) {
    let done = completed_files
        .fetch_add(1, Ordering::Relaxed)
        .checked_add(1)
        .expect("completed_files overflow");
    if done == 1 || done == total_files || done.is_multiple_of(100) {
        eprintln!("[arc-evm-specs-tests] processed {done}/{total_files} fixture files");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{bytes, LogData};
    use reth_evm::EvmEnv;
    use revm::context::result::{
        EVMError, ExecutionResult, HaltReason, InvalidTransaction, OutOfGasError, Output,
        SuccessReason,
    };
    use revm::database::{EmptyDB, State};
    use revm::state::AccountInfo;
    use revm_database::states::CacheState;
    use revm_statetest_types::{Env, Test, TestSuite, TransactionParts};
    use std::{
        collections::BTreeMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn touched_state(addresses: &[Address]) -> State<EmptyDB> {
        let mut cache = CacheState::new(true);
        for address in addresses {
            cache.insert_account(*address, AccountInfo::default());
        }
        State::builder()
            .with_cached_prestate(cache)
            .with_bundle_update()
            .build()
    }

    fn log_at(address: Address) -> revm_primitives::Log {
        revm_primitives::Log {
            address,
            data: LogData::new_unchecked(vec![B256::ZERO], bytes!("01")),
        }
    }

    fn fixture_test(expect_exception: Option<&str>) -> Test {
        serde_json::from_value(serde_json::json!({
            "expectException": expect_exception,
            "indexes": {
                "data": 0,
                "gas": 0,
                "value": 0
            },
            "hash": format!("{:#066x}", B256::ZERO),
            "postState": {},
            "logs": format!("{:#066x}", B256::ZERO)
        }))
        .expect("test fixture should deserialize")
    }

    fn fixture_unit(spec_name: SpecName) -> TestUnit {
        let mut post = BTreeMap::new();
        post.insert(spec_name, vec![fixture_test(None)]);

        TestUnit {
            info: None,
            env: Env {
                current_chain_id: Some(alloy_primitives::U256::from(1)),
                current_coinbase: Address::ZERO,
                current_difficulty: alloy_primitives::U256::ZERO,
                current_gas_limit: alloy_primitives::U256::from(30_000_000),
                current_number: alloy_primitives::U256::from(1),
                current_timestamp: alloy_primitives::U256::from(1),
                current_base_fee: Some(alloy_primitives::U256::ZERO),
                previous_hash: None,
                current_random: None,
                current_beacon_root: None,
                current_withdrawals_root: None,
                current_excess_blob_gas: None,
            },
            pre: alloy_primitives::map::HashMap::default(),
            post,
            transaction: TransactionParts {
                tx_type: None,
                data: vec![bytes!("")],
                gas_limit: vec![alloy_primitives::U256::from(21_000)],
                gas_price: Some(alloy_primitives::U256::from(1)),
                nonce: alloy_primitives::U256::ZERO,
                secret_key: B256::ZERO,
                sender: Some(Address::ZERO),
                to: Some(Address::ZERO),
                value: vec![alloy_primitives::U256::ZERO],
                max_fee_per_gas: None,
                max_priority_fee_per_gas: None,
                initcodes: None,
                access_lists: vec![],
                authorization_list: None,
                blob_versioned_hashes: vec![],
                max_fee_per_blob_gas: None,
            },
            out: None,
        }
    }

    fn fixture_context<'a>(
        unit: &'a TestUnit,
        test: &'a Test,
        evm_env: &'a EvmEnv,
        test_id: &'a str,
    ) -> TestExecutionContext<'a> {
        let factory =
            crate::adapter::build_evm_factory(crate::adapter::build_default_arc_chain_spec());
        let cache_state = CacheState::new(true);

        TestExecutionContext {
            factory: Box::leak(Box::new(factory)),
            cache_state: Box::leak(Box::new(cache_state)),
            evm_env,
            unit,
            test,
            test_id,
        }
    }

    fn call_result(output: Bytes) -> ExecutionResult<HaltReason> {
        ExecutionResult::Success {
            reason: SuccessReason::Return,
            gas_used: 21_000,
            gas_refunded: 0,
            logs: Vec::new(),
            output: Output::Call(output),
        }
    }

    fn revert_result(output: Bytes) -> ExecutionResult<HaltReason> {
        ExecutionResult::Revert {
            gas_used: 21_000,
            output,
        }
    }

    #[test]
    fn detects_arc_authority_log_signals() {
        let signals = summarize_arc_signals(
            &[log_at(ARC_NATIVE_COIN_AUTHORITY)],
            &touched_state(&[]),
            None,
            Address::ZERO,
        );

        assert!(signals.contains(SIGNAL_ARC_NATIVE_COIN_AUTHORITY_LOG_PRESENT));
    }

    #[test]
    fn detects_native_coin_control_touched_signals() {
        let state = touched_state(&[ARC_NATIVE_COIN_CONTROL]);
        let signals = summarize_arc_signals(&[], &state, None, Address::ZERO);

        assert!(signals.contains(SIGNAL_ARC_NATIVE_COIN_CONTROL_STATE_TOUCHED));
        assert!(signals.contains(SIGNAL_ARC_SYSTEM_ACCOUNT_TOUCHED));
    }

    #[test]
    fn detects_touched_precompile_signal() {
        let state = touched_state(&[address!("000000000000000000000000000000000000000a")]);
        let signals = summarize_arc_signals(&[], &state, None, Address::ZERO);

        assert!(signals
            .contains("precompile_address_touched=0x000000000000000000000000000000000000000A"));
    }

    #[test]
    fn ignores_non_precompile_low_address_signal() {
        let state = touched_state(&[address!("000000000000000000000000000000000000006a")]);
        let signals = summarize_arc_signals(&[], &state, None, Address::ZERO);

        assert!(!signals.contains(SIGNAL_PRECOMPILE_ADDRESS_TOUCHED));
    }

    #[test]
    fn detects_p256_precompile_signal() {
        let state = touched_state(&[address!("0000000000000000000000000000000000000100")]);
        let signals = summarize_arc_signals(&[], &state, None, Address::ZERO);

        assert!(signals
            .contains("precompile_address_touched=0x0000000000000000000000000000000000000100"));
    }

    #[test]
    fn detects_fixture_oracle_match_signal() {
        let root = B256::repeat_byte(0x11);
        let signals =
            summarize_arc_signals(&[], &touched_state(&[]), Some((root, root)), Address::ZERO);

        assert!(signals.contains(SIGNAL_FIXTURE_ORACLE_ROOT_MATCHES_FIXTURE_HASH));
    }

    #[test]
    fn filter_matching_does_not_use_substrings() {
        let fixture_name = "foo_bar_1";
        let test_id = "foo_bar_1/Prague/d0_g0_v0";

        assert!(matches_filter(
            fixture_name,
            test_id,
            fixture_name,
            &SpecName::Prague
        ));
        assert!(!matches_filter(
            "foo_bar_10",
            test_id,
            fixture_name,
            &SpecName::Prague
        ));
    }

    #[test]
    fn output_name_prefers_fixture_filter_variants() {
        let test_id = "fixture/Prague/d0_g0_v0";
        let fixture_name = "./fixture";

        assert_eq!(
            output_name_for_filter(Some("fixture"), test_id, fixture_name, &SpecName::Prague),
            test_id
        );
        assert_eq!(
            output_name_for_filter(
                Some("fixture[fork_Prague-state_test]"),
                test_id,
                "fixture",
                &SpecName::Prague
            ),
            "fixture[fork_Prague-state_test]"
        );
        assert_eq!(
            output_name_for_filter(None, test_id, fixture_name, &SpecName::Prague),
            fixture_name
        );
    }

    #[test]
    fn summarize_logs_and_touched_accounts_produce_previews() {
        let log_summary = summarize_logs(&[log_at(ARC_NATIVE_COIN_AUTHORITY)]);
        assert!(log_summary.contains("first_log=address="));
        assert!(log_summary.contains("topics=1"));

        let state = touched_state(&[ARC_NATIVE_COIN_CONTROL]);
        let touched_summary = summarize_touched_accounts(&state);
        assert!(touched_summary.contains("0x1800000000000000000000000000000000000001"));
        assert!(touched_summary.contains("storage_slots=0"));
    }

    #[test]
    fn execute_test_suite_records_missing_chain_id_as_failures() {
        let mut unit = fixture_unit(SpecName::Prague);
        unit.env.current_chain_id = None;
        let suite = TestSuite(BTreeMap::from([(String::from("fixture"), unit)]));
        let factory =
            crate::adapter::build_evm_factory(crate::adapter::build_default_arc_chain_spec());

        let (results, summary) =
            execute_test_suite(&suite, &factory, &HashMap::new(), None, false, false);

        assert_eq!(summary.tests_total, 1);
        assert_eq!(summary.tests_failed, 1);
        assert_eq!(results.len(), 1);
        assert!(results[0].error.contains("Missing chain_id"));
    }

    #[test]
    fn execute_test_suite_skips_unknown_specs() {
        let suite = TestSuite(BTreeMap::from([(
            String::from("fixture"),
            fixture_unit(SpecName::Unknown),
        )]));
        let factory =
            crate::adapter::build_evm_factory(crate::adapter::build_default_arc_chain_spec());

        let (results, summary) =
            execute_test_suite(&suite, &factory, &HashMap::new(), None, false, false);

        assert!(results.is_empty());
        assert_eq!(summary.tests_total, 0);
        assert_eq!(summary.tests_failed, 0);
        assert_eq!(summary.tests_skipped_by_spec, 1);
    }

    #[test]
    fn execute_test_suite_reports_tx_env_build_failures() {
        let mut unit = fixture_unit(SpecName::Prague);
        unit.transaction.to = None;
        unit.transaction.max_fee_per_blob_gas = Some(alloy_primitives::U256::from(1));
        let suite = TestSuite(BTreeMap::from([(String::from("fixture"), unit)]));
        let factory =
            crate::adapter::build_evm_factory(crate::adapter::build_default_arc_chain_spec());

        let (results, summary) =
            execute_test_suite(&suite, &factory, &HashMap::new(), None, false, false);

        assert_eq!(summary.tests_total, 1);
        assert_eq!(summary.tests_failed, 1);
        assert_eq!(results.len(), 1);
        assert!(results[0].error.contains("HARNESS_PRECONDITION"));
        assert!(results[0].error.contains("TX_ENV_BUILD_FAILED"));
    }

    #[test]
    fn handle_tx_env_error_accepts_fixture_declared_exception() {
        let unit = fixture_unit(SpecName::Prague);
        let test = fixture_test(Some("PriorityGreaterThanMaxFeePerGas"));
        let evm_env = crate::adapter::build_evm_env(&unit, &SpecName::Prague, 1)
            .expect("test env should build");
        let ctx = fixture_context(&unit, &test, &evm_env, "fixture/Prague/d0_g0_v0");

        let result = handle_tx_env_error(
            &ctx,
            "tx env build failed: got Some(\"priority fee is greater than max fee\")",
        );

        assert!(result.is_ok());
    }

    #[test]
    fn handle_tx_env_error_reports_harness_precondition_without_expected_exception() {
        let unit = fixture_unit(SpecName::Prague);
        let test = fixture_test(None);
        let evm_env = crate::adapter::build_evm_env(&unit, &SpecName::Prague, 1)
            .expect("test env should build");
        let ctx = fixture_context(&unit, &test, &evm_env, "fixture/Prague/d0_g0_v0");

        let error = handle_tx_env_error(&ctx, "tx env build failed: missing to").unwrap_err();
        let rendered = error.to_string();

        assert!(rendered.contains("HARNESS_PRECONDITION"));
        assert!(rendered.contains("TX_ENV_BUILD_FAILED"));
        assert!(rendered.contains("missing to"));
    }

    #[test]
    fn validate_expected_exception_distinguishes_expected_and_unexpected_outcomes() {
        let unit = fixture_unit(SpecName::Prague);
        let evm_env = crate::adapter::build_evm_env(&unit, &SpecName::Prague, 1)
            .expect("test env should build");

        let matching_test = fixture_test(Some("PriorityGreaterThanMaxFeePerGas"));
        let matching_ctx =
            fixture_context(&unit, &matching_test, &evm_env, "fixture/Prague/d0_g0_v0");
        let matching_error = Err(EVMError::Transaction(
            InvalidTransaction::PriorityFeeGreaterThanMaxFee,
        ));
        assert!(validate_expected_exception(&matching_ctx, &matching_error)
            .expect("expected exception should match"));

        let unexpected_success_test = fixture_test(Some("PriorityGreaterThanMaxFeePerGas"));
        let unexpected_success_ctx = fixture_context(
            &unit,
            &unexpected_success_test,
            &evm_env,
            "fixture/Prague/d0_g0_v0",
        );
        let unexpected_success =
            validate_expected_exception(&unexpected_success_ctx, &Ok(call_result(bytes!("01"))))
                .unwrap_err()
                .to_string();
        assert!(unexpected_success.contains("UNEXPECTED_SUCCESS"));

        let unexpected_exception_test = fixture_test(None);
        let unexpected_exception_ctx = fixture_context(
            &unit,
            &unexpected_exception_test,
            &evm_env,
            "fixture/Prague/d0_g0_v0",
        );
        let unexpected_exception = validate_expected_exception(
            &unexpected_exception_ctx,
            &Err(EVMError::Transaction(
                InvalidTransaction::PriorityFeeGreaterThanMaxFee,
            )),
        )
        .unwrap_err()
        .to_string();
        assert!(unexpected_exception.contains("UNEXPECTED_EXCEPTION"));
    }

    #[test]
    fn validate_output_flags_only_mismatched_call_data() {
        let mut unit = fixture_unit(SpecName::Prague);
        unit.out = Some(bytes!("0102"));
        let test = fixture_test(None);
        let evm_env = crate::adapter::build_evm_env(&unit, &SpecName::Prague, 1)
            .expect("test env should build");
        let ctx = fixture_context(&unit, &test, &evm_env, "fixture/Prague/d0_g0_v0");

        validate_output(&ctx, unit.out.as_ref(), &call_result(bytes!("0102")))
            .expect("matching output should pass");

        let error = validate_output(&ctx, unit.out.as_ref(), &call_result(bytes!("03")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("UNEXPECTED_OUTPUT"));

        validate_output(
            &ctx,
            unit.out.as_ref(),
            &ExecutionResult::Halt {
                reason: HaltReason::OutOfGas(OutOfGasError::Basic),
                gas_used: 21_000,
            },
        )
        .expect("halted executions do not have output to compare");
    }

    #[test]
    fn configure_blob_limits_tracks_fork_specific_limits() {
        let mut pre_prague = CfgEnv::default();
        pre_prague.spec = SpecId::CANCUN;
        configure_blob_limits(&mut pre_prague);
        assert_eq!(pre_prague.max_blobs_per_tx, Some(6));

        let mut prague = CfgEnv::default();
        prague.spec = SpecId::PRAGUE;
        configure_blob_limits(&mut prague);
        assert_eq!(prague.max_blobs_per_tx, Some(9));

        let mut osaka = CfgEnv::default();
        osaka.spec = SpecId::OSAKA;
        configure_blob_limits(&mut osaka);
        assert_eq!(osaka.max_blobs_per_tx, Some(6));
    }

    #[test]
    fn format_evm_result_preserves_result_kind_for_json_consumers() {
        assert_eq!(
            format_evm_result(&Ok(call_result(bytes!("010203")))),
            "Success: Return"
        );
        assert_eq!(
            format_evm_result(&Ok(revert_result(bytes!("deadbeef")))),
            "Revert"
        );
        assert_eq!(
            format_evm_result(&Ok(ExecutionResult::Halt {
                reason: HaltReason::OutOfGas(OutOfGasError::Basic),
                gas_used: 21_000,
            })),
            "Halt: OutOfGas(Basic)"
        );
        assert_eq!(
            format_evm_result(&Err(EVMError::Transaction(
                InvalidTransaction::PriorityFeeGreaterThanMaxFee,
            ))),
            "transaction validation error: priority fee is greater than max fee"
        );
    }

    #[test]
    fn json_outcome_report_preserves_legacy_aliases_and_readable_index_names() {
        let outcome = JsonOutcome {
            state_root: B256::repeat_byte(0x11),
            logs_root: B256::repeat_byte(0x22),
            output: bytes!("0102"),
            gas_used: 21_000,
            error_msg: "boom".to_string(),
            evm_result: "Success: Return".to_string(),
            post_logs_hash: B256::repeat_byte(0x33),
            fork: "BERLIN".to_string(),
            test: "fixture/Berlin/d0_g0_v0".to_string(),
            data_index: 0,
            gas_index: 1,
            value_index: 2,
        };

        let report = build_json_outcome_report(&outcome, false);

        assert_eq!(report.get("pass").unwrap(), false);
        assert_eq!(report.get("gasUsed").unwrap(), 21_000);
        assert_eq!(report.get("fork").unwrap(), "BERLIN");
        assert_eq!(report.get("errorMsg").unwrap(), "boom");
        assert_eq!(report.get("d").unwrap(), 0);
        assert_eq!(report.get("g").unwrap(), 1);
        assert_eq!(report.get("v").unwrap(), 2);
        assert_eq!(report.get("data_index").unwrap(), 0);
        assert_eq!(report.get("gas_index").unwrap(), 1);
        assert_eq!(report.get("value_index").unwrap(), 2);
    }

    #[test]
    fn file_errors_are_counted_in_summary() {
        let output = file_error_result(Path::new("/tmp/bad.json"), "boom".to_string());

        assert_eq!(output.summary.tests_total, 0);
        assert_eq!(output.summary.tests_failed, 0);
        assert_eq!(output.fatal_file_errors, 1);
    }

    #[test]
    fn malformed_fixture_shape_becomes_file_error() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file = std::env::temp_dir().join(format!("arc_evm_specs_tests_bad_shape_{nonce}.json"));
        std::fs::write(
            &file,
            serde_json::json!({
                "my_test": {
                    "env": {},
                    "post": {}
                }
            })
            .to_string(),
        )
        .unwrap();

        let completed = AtomicUsize::new(0);
        let output = process_fixture_file(&file, None, false, false, &completed, 1);

        assert_eq!(output.fatal_file_errors, 1);
        assert_eq!(output.summary.files_processed, 1);
        assert_eq!(output.summary.tests_total, 0);
        assert_eq!(output.results.len(), 1);
        assert!(output.results[0]
            .error
            .contains("unsupported or malformed statetest fixture shape"));

        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn looks_like_statetest_fixture_checks_required_keys() {
        let good = serde_json::json!({
            "my_test": {
                "env": {},
                "post": {},
                "pre": {}
            }
        });
        let bad = serde_json::json!({
            "config": { "name": "not-a-fixture" }
        });
        assert!(looks_like_statetest_fixture(&good));
        assert!(!looks_like_statetest_fixture(&bad));
    }

    #[test]
    fn find_json_files_recurses_and_filters_extensions() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("arc_evm_specs_tests_find_json_{nonce}"));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let a = root.join("a.json");
        let b = nested.join("b.json");
        let c = nested.join("c.txt");
        std::fs::write(&a, "{}").unwrap();
        std::fs::write(&b, "{}").unwrap();
        std::fs::write(&c, "ignore").unwrap();

        let mut files = find_json_files(&root).unwrap();
        files.sort();

        assert_eq!(files, vec![a, b]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn run_returns_no_json_files_for_empty_directory() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("arc_evm_specs_tests_empty_{nonce}"));
        std::fs::create_dir_all(&root).unwrap();

        let err =
            run(root.clone(), None, false, false, false).expect_err("empty directory should fail");
        assert!(matches!(
            err,
            EvmSpecsTestError::NoJsonFiles { path } if path == root.display().to_string()
        ));

        std::fs::remove_dir_all(root).unwrap();
    }
}
