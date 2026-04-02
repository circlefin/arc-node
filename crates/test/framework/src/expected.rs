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

//! Flexible assertion helpers for decision/block counts.

use core::fmt;

/// Expectation on a count (decisions, blocks, etc.) that supports flexible matching.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Expected {
    /// Exactly `n`.
    Exactly(usize),
    /// At least `n`.
    AtLeast(usize),
    /// At most `n`.
    AtMost(usize),
    /// Strictly less than `n`.
    LessThan(usize),
    /// Strictly greater than `n`.
    GreaterThan(usize),
}

impl Expected {
    /// Check whether `actual` satisfies this expectation.
    pub fn check(&self, actual: usize) -> bool {
        match self {
            Self::Exactly(n) => actual == *n,
            Self::AtLeast(n) => actual >= *n,
            Self::AtMost(n) => actual <= *n,
            Self::LessThan(n) => actual < *n,
            Self::GreaterThan(n) => actual > *n,
        }
    }
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exactly(n) => write!(f, "exactly {n}"),
            Self::AtLeast(n) => write!(f, "at least {n}"),
            Self::AtMost(n) => write!(f, "at most {n}"),
            Self::LessThan(n) => write!(f, "less than {n}"),
            Self::GreaterThan(n) => write!(f, "greater than {n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Expected;

    #[test]
    fn expected_exactly() {
        assert!(Expected::Exactly(5).check(5));
        assert!(!Expected::Exactly(5).check(4));
        assert!(!Expected::Exactly(5).check(6));
    }

    #[test]
    fn expected_at_most() {
        assert!(Expected::AtMost(5).check(5));
        assert!(Expected::AtMost(5).check(3));
        assert!(!Expected::AtMost(5).check(6));
    }

    #[test]
    fn expected_less_than() {
        assert!(Expected::LessThan(5).check(4));
        assert!(!Expected::LessThan(5).check(5));
        assert!(!Expected::LessThan(5).check(6));
    }

    #[test]
    fn expected_greater_than() {
        assert!(Expected::GreaterThan(5).check(6));
        assert!(!Expected::GreaterThan(5).check(5));
        assert!(!Expected::GreaterThan(5).check(4));
    }

    #[test]
    fn expected_at_least() {
        assert!(Expected::AtLeast(5).check(5));
        assert!(Expected::AtLeast(5).check(6));
        assert!(!Expected::AtLeast(5).check(4));
    }
}
