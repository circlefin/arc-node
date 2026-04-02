// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use alloy_signer_local::{coins_bip39::English, LocalSigner, MnemonicBuilder};
use clap::{builder::PossibleValue, ValueEnum};
use color_eyre::eyre::{self, Context, Result};
use k256::ecdsa::SigningKey;
use strum_macros::EnumString;

#[derive(Clone)]
pub(crate) struct AccountBuilder {
    mnemonic: String,
}

impl AccountBuilder {
    pub fn new(mnemonic: String) -> Self {
        Self { mnemonic }
    }

    pub fn build(&self, index: usize) -> Result<LocalSigner<SigningKey>> {
        let builder = MnemonicBuilder::<English>::default()
            .phrase(&self.mnemonic)
            .derivation_path(format!("m/44'/60'/1'/0/{}", index))
            .with_context(|| format!("Failed to build derivation path for index={index}"))?;
        builder.build().with_context(|| "Failed to create wallet")
    }
}

#[derive(Clone, Copy, Debug, EnumString, Default)]
#[strum(serialize_all = "lowercase")]
pub enum PartitionMode {
    #[default]
    Linear,
    Exponential,
}

impl PartitionMode {
    /// Partition the account space [0, num_accounts) into `num_generators` contiguous ranges according to the chosen mode.
    /// Returns an ordered Vec of (start, end) pairs, end-exclusive.
    pub(crate) fn partition_accounts(
        &self,
        num_accounts: usize,
        num_generators: usize,
    ) -> Result<Vec<(usize, usize)>> {
        if num_generators == 0 {
            eyre::bail!("num_generators must be greater than 0");
        }
        if num_accounts < num_generators {
            eyre::bail!("num_accounts must be greater than or equal to num_generators");
        }
        match self {
            PartitionMode::Linear => Self::partition_linear(num_accounts, num_generators),
            PartitionMode::Exponential => Self::partition_exponential(num_accounts, num_generators),
        }
    }

    // Evenly spread, each gets floor, except the last one which gets all that's left
    fn partition_linear(num_accounts: usize, num_generators: usize) -> Result<Vec<(usize, usize)>> {
        let base = num_accounts / num_generators;
        let remainder = num_accounts % num_generators;
        let mut ranges = Vec::with_capacity(num_generators);
        let mut start = 0;
        for i in 0..num_generators {
            let is_last = i == num_generators - 1;
            let size = base + if is_last { remainder } else { 0 };
            let end = start + size;
            ranges.push((start, end));
            start = end;
        }
        Ok(ranges)
    }

    /// Partition the account space of size `num_accounts` into `num_generators`
    /// buckets by splitting the space in half repeatedly.
    ///
    /// 1 generator would result in a range [0, N), where N = `num_accounts`
    /// 2 generators would result in ranges [0, N/2), [N/2, N)
    /// 3 generators would result in ranges [0, N/4), [N/4, N/2), [N/2, N)
    /// 4 generators would result in ranges [0, N/8), [N/8, N/4), [N/4, N/2), [N/2, N)
    /// ...
    /// G generators would result in ranges [0, N/2^(G-1)), [N/2^(G-1), N/2^(G-2)), ..., [N/2, N)
    fn partition_exponential(
        num_accounts: usize,
        num_generators: usize,
    ) -> Result<Vec<(usize, usize)>> {
        fn round_div(n: usize, d: usize) -> usize {
            (n + d / 2) / d
        }

        if round_div(num_accounts, 1 << (num_generators - 1)) == 0 {
            eyre::bail!("too many generators: it would result in a bucket with size 0");
        }

        // Build boundaries as described in the doc comment
        let mut boundaries: Vec<usize> = Vec::with_capacity(num_generators);
        if num_generators == 1 {
            boundaries.push(num_accounts);
        } else {
            for j in (1..=num_generators - 1).rev() {
                let denom = 1usize << j;
                boundaries.push(round_div(num_accounts, denom));
            }
            boundaries.push(num_accounts);
        }

        // Convert boundaries to contiguous ranges (pairs)
        let mut ranges = Vec::with_capacity(num_generators);
        let mut start = 0usize;
        for end in boundaries {
            ranges.push((start, end));
            start = end;
        }
        Ok(ranges)
    }
}

impl ValueEnum for PartitionMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Linear, Self::Exponential]
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        match self {
            Self::Linear => Some(PossibleValue::new("linear").help("Evenly partition accounts")),
            Self::Exponential => Some(
                PossibleValue::new("exponential")
                    .help("Exponentially partition accounts to favor lower indices"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partition_accounts_linear() -> Result<()> {
        #[rustfmt::skip]
        let test_cases = vec![
            (10, 0, None),
            (10, 1, Some(vec![(0, 10)])),
            (10, 3, Some(vec![(0, 3), (3, 6), (6, 10)])),
            (10, 4, Some(vec![(0, 2), (2, 4), (4, 6), (6, 10)])),
            (10, 5, Some(vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)])),
            (10, 8, Some(vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7), (7, 10)])),
            (10, 10, Some(vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7), (7, 8), (8, 9), (9, 10)])),
            (10, 11, None),
        ];
        for (num_accounts, num_generators, expected) in test_cases {
            let ranges = PartitionMode::Linear.partition_accounts(num_accounts, num_generators);
            assert_eq!(ranges.ok(), expected);
        }
        Ok(())
    }

    #[tokio::test]
    async fn partition_accounts_exponential() -> Result<()> {
        #[rustfmt::skip]
        let test_cases = vec![
            (100, 0, None),
            (100, 1, Some(vec![(0, 100)])),
            (100, 2, Some(vec![(0, 50), (50, 100)])),
            (100, 3, Some(vec![(0, 25), (25, 50), (50, 100)])),
            (100, 4, Some(vec![(0, 13), (13, 25), (25, 50), (50, 100)])),
            (100, 5, Some(vec![(0, 6), (6, 13), (13, 25), (25, 50), (50, 100)])),
            (100, 8, Some(vec![(0, 1), (1, 2), (2, 3), (3, 6), (6, 13), (13, 25), (25, 50), (50, 100)])),
            (100, 9, None),
        ];
        for (num_accounts, num_generators, expected) in test_cases {
            let ranges =
                PartitionMode::Exponential.partition_accounts(num_accounts, num_generators);
            assert_eq!(ranges.ok(), expected);
        }
        Ok(())
    }
}
