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

//! Temporary exception-bucket matching helpers.
//!
//! This module is intentionally stricter than upstream `revme` today.
//! `revme` currently treats any execution error as satisfying
//! `expectException`, and during tx env construction it also accepts:
//!
//! ```ignore
//! let tx = match test.tx_env(&unit) {
//!     Ok(tx) => tx,
//!     Err(_) if test.expect_exception.is_some() => continue,
//!     Err(e) => ...
//! };
//! ```
//!
//! See `bins/revme/src/cmd/statetest/runner.rs` in the `bluealloy/revm`
//! repository.
//!
//! We are not following that behavior yet because this stage of the work is
//! focused on debugging expectation mismatches across different error buckets.
//! For that reason, this file normalizes and classifies error strings so the
//! runner can distinguish "wrong exception bucket" from "some exception
//! happened".
//!
//! Once that debugging stage is complete, we should remove this stricter
//! bucket-matching behavior and follow `revme`'s exception handling semantics.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExceptionCategory {
    IntrinsicGasTooLow,
    IntrinsicGasBelowFloorGasCost,
    InsufficientAccountFunds,
    GasLimitPriceProductOverflow,
    SenderNotEoa,
    Type3TxBlobCountExceeded,
    Type1TxPreFork,
    Type2TxPreFork,
    Type3TxInvalidBlobVersionedHash,
    Type3TxZeroBlobs,
    Type4EmptyAuthorizationList,
    Type4TxPreFork,
    InsufficientMaxFeePerGas,
    InsufficientMaxFeePerBlobGas,
    InitcodeSizeExceeded,
    GasLimitExceedsMaximum,
    PriorityGreaterThanMaxFeePerGas,
    GasAllowanceExceeded,
}

pub(crate) fn exception_matches(expected: &str, actual: &str) -> bool {
    let actual_norm = normalize_exception_phrase(actual);
    if actual_norm.is_empty() {
        return false;
    }

    let actual_category = actual_exception_category(&actual_norm);
    expected
        .split('|')
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .any(|candidate| expected_alternative_matches(candidate, &actual_norm, actual_category))
}

pub(crate) fn normalize_exception_phrase(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut prev_was_alnum = false;
    let mut prev_was_lower = false;
    let mut prev_was_digit = false;

    for ch in value.chars() {
        if !ch.is_ascii_alphanumeric() {
            normalized.push(' ');
            prev_was_alnum = false;
            prev_was_lower = false;
            prev_was_digit = false;
            continue;
        }

        let is_upper = ch.is_ascii_uppercase();
        let is_lower = ch.is_ascii_lowercase();
        let is_digit = ch.is_ascii_digit();

        if prev_was_alnum
            && ((prev_was_lower && is_upper)
                || (prev_was_digit && !is_digit)
                || (!prev_was_digit && is_digit))
        {
            normalized.push(' ');
        }

        normalized.push(ch.to_ascii_lowercase());
        prev_was_alnum = true;
        prev_was_lower = is_lower;
        prev_was_digit = is_digit;
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn tx_env_actual_exception(error: &str) -> Option<&str> {
    let got_marker = "got Some(\"";
    let start = error.find(got_marker)?.checked_add(got_marker.len())?;
    let rest = &error[start..];
    let end = rest.find("\")")?;
    Some(&rest[..end])
}

pub(crate) fn tx_env_exception_matches(expected: &str, actual: &str) -> bool {
    if exception_matches(expected, actual) {
        return true;
    }

    let expected_norm = normalize_exception_phrase(expected);
    let actual_norm = normalize_exception_phrase(actual);

    let expected_key = normalize_expected_key(&expected_norm);
    matches!(
        (expected_key.as_ref(), actual_norm.as_str()),
        ("type 3 tx contract creation", "invalid transaction type")
            | ("type 4 tx contract creation", "invalid transaction type")
    )
}

fn normalize_expected_key(expected_norm: &str) -> Cow<'_, str> {
    let trimmed = expected_norm
        .trim_start_matches("transaction exception ")
        .trim_start_matches("exception ")
        .trim();
    Cow::Borrowed(trimmed)
}

fn expected_alternative_matches(
    expected: &str,
    actual_norm: &str,
    actual_category: Option<ExceptionCategory>,
) -> bool {
    let expected_norm = normalize_exception_phrase(expected);
    if expected_norm.is_empty() {
        return false;
    }

    if let Some(expected_category) = expected_exception_category(&expected_norm) {
        return actual_category.is_some_and(|actual| actual == expected_category);
    }

    if expected_norm == actual_norm {
        return true;
    }
    if expected_norm.split_whitespace().count() < 2 {
        return false;
    }

    let bounded_actual = format!(" {actual_norm} ");
    let bounded_expected = format!(" {expected_norm} ");
    bounded_actual.contains(&bounded_expected)
}

fn expected_exception_category(expected_norm: &str) -> Option<ExceptionCategory> {
    match normalize_expected_key(expected_norm).as_ref() {
        "intrinsic gas too low" => Some(ExceptionCategory::IntrinsicGasTooLow),
        "intrinsic gas below floor gas cost" => {
            Some(ExceptionCategory::IntrinsicGasBelowFloorGasCost)
        }
        "insufficient account funds" => Some(ExceptionCategory::InsufficientAccountFunds),
        "gaslimit price product overflow" => Some(ExceptionCategory::GasLimitPriceProductOverflow),
        "sender not eoa" => Some(ExceptionCategory::SenderNotEoa),
        "type 3 tx max blob gas allowance exceeded" | "type 3 tx blob count exceeded" => {
            Some(ExceptionCategory::Type3TxBlobCountExceeded)
        }
        "type 1 tx pre fork" => Some(ExceptionCategory::Type1TxPreFork),
        "type 2 tx pre fork" => Some(ExceptionCategory::Type2TxPreFork),
        "type 3 tx invalid blob versioned hash" => {
            Some(ExceptionCategory::Type3TxInvalidBlobVersionedHash)
        }
        "type 3 tx zero blobs" => Some(ExceptionCategory::Type3TxZeroBlobs),
        "type 4 empty authorization list" => Some(ExceptionCategory::Type4EmptyAuthorizationList),
        "type 4 tx pre fork" => Some(ExceptionCategory::Type4TxPreFork),
        "insufficient max fee per gas" => Some(ExceptionCategory::InsufficientMaxFeePerGas),
        "insufficient max fee per blob gas" => {
            Some(ExceptionCategory::InsufficientMaxFeePerBlobGas)
        }
        "initcode size exceeded" => Some(ExceptionCategory::InitcodeSizeExceeded),
        "gas limit exceeds maximum" => Some(ExceptionCategory::GasLimitExceedsMaximum),
        "priority greater than max fee per gas" => {
            Some(ExceptionCategory::PriorityGreaterThanMaxFeePerGas)
        }
        "gas allowance exceeded" => Some(ExceptionCategory::GasAllowanceExceeded),
        _ => None,
    }
}

fn actual_exception_category(actual_norm: &str) -> Option<ExceptionCategory> {
    actual_exception_rules()
        .iter()
        .find(|rule| rule.matches(actual_norm))
        .map(|rule| rule.category)
}

struct ActualExceptionRule {
    category: ExceptionCategory,
    required_substrings: &'static [&'static str],
}

impl ActualExceptionRule {
    fn matches(&self, actual_norm: &str) -> bool {
        self.required_substrings
            .iter()
            .all(|needle| actual_norm.contains(needle))
    }
}

fn actual_exception_rules() -> &'static [ActualExceptionRule] {
    &[
        ActualExceptionRule {
            category: ExceptionCategory::IntrinsicGasTooLow,
            required_substrings: &["call gas cost", "exceeds the gas limit"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::IntrinsicGasBelowFloorGasCost,
            required_substrings: &["gas floor", "exceeds the gas limit"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::InsufficientAccountFunds,
            required_substrings: &["lack of funds", "for max fee"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::GasLimitPriceProductOverflow,
            required_substrings: &["overflow payment in transaction"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::SenderNotEoa,
            required_substrings: &["senders with deployed code"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type3TxBlobCountExceeded,
            required_substrings: &["too many blobs"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type1TxPreFork,
            required_substrings: &["eip 2930", "not supported"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type2TxPreFork,
            required_substrings: &["eip 1559", "not supported"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type3TxInvalidBlobVersionedHash,
            required_substrings: &["blob version not supported"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type3TxZeroBlobs,
            required_substrings: &["empty blobs"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type4EmptyAuthorizationList,
            required_substrings: &["empty authorization list"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::Type4TxPreFork,
            required_substrings: &["eip 7702", "not supported"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::InsufficientMaxFeePerGas,
            required_substrings: &["gas price is less than basefee"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::InsufficientMaxFeePerBlobGas,
            required_substrings: &["blob gas price", "greater than max fee per blob gas"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::InitcodeSizeExceeded,
            required_substrings: &["create initcode size limit"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::GasLimitExceedsMaximum,
            required_substrings: &["transaction gas limit", "greater than the cap"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::PriorityGreaterThanMaxFeePerGas,
            required_substrings: &["priority fee is greater than max fee"],
        },
        ActualExceptionRule {
            category: ExceptionCategory::GasAllowanceExceeded,
            required_substrings: &["caller gas limit exceeds the block gas limit"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_matching_requires_the_expected_error() {
        assert!(exception_matches(
            "TR_TypeNotSupported",
            "transaction validation error: tr type not supported"
        ));
        assert!(!exception_matches(
            "TR_TypeNotSupported",
            "transaction validation error: nonce too low"
        ));
    }

    #[test]
    fn exception_matching_rejects_substring_word_matches() {
        assert!(!exception_matches("gas", "IntrinsicGasTooLow"));
    }

    #[test]
    fn exception_matching_accepts_expected_alternatives() {
        assert!(exception_matches(
            "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS|TransactionException.INTRINSIC_GAS_TOO_LOW",
            "transaction validation error: call gas cost (53000) exceeds the gas limit (21000)"
        ));
    }

    #[test]
    fn exception_matching_keeps_unrecognized_exact_alternative() {
        assert!(exception_matches(
            "TransactionException.INTRINSIC_GAS_TOO_LOW|custom exact mismatch text",
            "custom exact mismatch text"
        ));
    }

    #[test]
    fn exception_matching_maps_high_volume_transaction_aliases() {
        let cases = [
            (
                "TransactionException.INTRINSIC_GAS_TOO_LOW",
                "transaction validation error: call gas cost (53000) exceeds the gas limit (21000)",
            ),
            (
                "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS",
                "transaction validation error: lack of funds (100) for max fee (101)",
            ),
            (
                "TransactionException.INSUFFICIENT_ACCOUNT_FUNDS|TransactionException.GASLIMIT_PRICE_PRODUCT_OVERFLOW",
                "transaction validation error: overflow payment in transaction",
            ),
            (
                "TransactionException.INTRINSIC_GAS_BELOW_FLOOR_GAS_COST",
                "transaction validation error: gas floor (53000) exceeds the gas limit (21000)",
            ),
            (
                "TransactionException.SENDER_NOT_EOA",
                "transaction validation error: reject transactions from senders with deployed code",
            ),
            (
                "TransactionException.TYPE_2_TX_PRE_FORK",
                "transaction validation error: Eip1559 is not supported",
            ),
            (
                "TransactionException.TYPE_1_TX_PRE_FORK",
                "transaction validation error: Eip2930 is not supported",
            ),
            (
                "TransactionException.TYPE_4_TX_PRE_FORK",
                "transaction validation error: Eip7702 is not supported",
            ),
            (
                "TransactionException.TYPE_3_TX_INVALID_BLOB_VERSIONED_HASH",
                "transaction validation error: blob version not supported",
            ),
            (
                "TransactionException.TYPE_3_TX_MAX_BLOB_GAS_ALLOWANCE_EXCEEDED|TransactionException.TYPE_3_TX_BLOB_COUNT_EXCEEDED",
                "transaction validation error: too many blobs, have 7, max 6",
            ),
            (
                "TransactionException.INSUFFICIENT_MAX_FEE_PER_GAS",
                "transaction validation error: gas price is less than basefee",
            ),
            (
                "TransactionException.INITCODE_SIZE_EXCEEDED",
                "transaction validation error: create initcode size limit exceeded",
            ),
            (
                "TransactionException.GAS_LIMIT_EXCEEDS_MAXIMUM",
                "transaction validation error: transaction gas limit 30000001 greater than the cap",
            ),
            (
                "TransactionException.PRIORITY_GREATER_THAN_MAX_FEE_PER_GAS",
                "transaction validation error: priority fee is greater than max fee",
            ),
            (
                "TransactionException.TYPE_3_TX_ZERO_BLOBS",
                "transaction validation error: empty blobs are not allowed",
            ),
            (
                "TransactionException.TYPE_4_EMPTY_AUTHORIZATION_LIST",
                "transaction validation error: empty authorization list",
            ),
            (
                "TransactionException.INSUFFICIENT_MAX_FEE_PER_BLOB_GAS",
                "transaction validation error: blob gas price 5 greater than max fee per blob gas 4",
            ),
            (
                "TransactionException.GAS_ALLOWANCE_EXCEEDED",
                "transaction validation error: caller gas limit exceeds the block gas limit",
            ),
        ];

        for (expected, actual) in cases {
            assert!(
                exception_matches(expected, actual),
                "expected {expected} to match {actual}"
            );
        }
    }

    #[test]
    fn normalize_exception_phrase_splits_camel_case_and_digits() {
        assert_eq!(
            normalize_exception_phrase("IntrinsicGasTooLow123"),
            "intrinsic gas too low 123"
        );
    }

    #[test]
    fn tx_env_actual_exception_extracts_inner_message() {
        assert_eq!(
            tx_env_actual_exception(
                "unexpected exception: got Some(\"Invalid transaction type\"), expected Some(\"TransactionException.TYPE_3_TX_CONTRACT_CREATION\")"
            ),
            Some("Invalid transaction type")
        );
    }

    #[test]
    fn tx_env_exception_matches_contract_creation_aliases() {
        assert!(tx_env_exception_matches(
            "TransactionException.TYPE_3_TX_CONTRACT_CREATION",
            "Invalid transaction type"
        ));
        assert!(tx_env_exception_matches(
            "TransactionException.TYPE_4_TX_CONTRACT_CREATION",
            "Invalid transaction type"
        ));
    }
}
