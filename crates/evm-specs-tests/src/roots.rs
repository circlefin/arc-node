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

use std::convert::Infallible;

use alloy_primitives::{address, keccak256, Address, B256, U256};
use alloy_rlp::{RlpEncodable, RlpMaxEncodedLen};
use alloy_trie::{
    root::{state_root_unhashed, storage_root_unhashed},
    TrieAccount as FixtureTrieAccount, EMPTY_ROOT_HASH,
};
use hash_db::Hasher;
use plain_hasher::PlainHasher;
use revm::{
    context::result::{EVMError, ExecutionResult, HaltReason, InvalidTransaction},
    database::{bal::EvmDatabaseError, EmptyDB, PlainAccount, State},
};
use revm_primitives::Log;
use revm_statetest_types::AccountInfo as FixtureAccountInfo;
use triehash::sec_trie_root;

pub struct TestValidationResult {
    pub logs_root: B256,
    pub state_root: B256,
}

const ARC_NATIVE_COIN_AUTHORITY: Address = address!("1800000000000000000000000000000000000000");

pub fn compute_test_roots(
    exec_result: &Result<
        ExecutionResult<HaltReason>,
        EVMError<EvmDatabaseError<Infallible>, InvalidTransaction>,
    >,
    db: &State<EmptyDB>,
) -> TestValidationResult {
    TestValidationResult {
        logs_root: compute_logs_hash(
            filter_validation_logs(
                exec_result
                    .as_ref()
                    .map(|result| result.logs())
                    .unwrap_or_default(),
            )
            .as_slice(),
        ),
        state_root: state_merkle_trie_root(db.cache.trie_account()),
    }
}

pub fn compute_logs_hash(logs: &[Log]) -> B256 {
    let mut encoded = Vec::new();
    alloy_rlp::encode_list(logs, &mut encoded);
    keccak256(&encoded)
}

pub fn filter_validation_logs(logs: &[Log]) -> Vec<Log> {
    logs.iter()
        .filter(|log| log.address != ARC_NATIVE_COIN_AUTHORITY)
        .cloned()
        .collect()
}

pub fn state_merkle_trie_root<'a>(
    accounts: impl IntoIterator<Item = (Address, &'a PlainAccount)>,
) -> B256 {
    trie_root(
        accounts
            .into_iter()
            .filter(|(_, account)| !is_empty_plain_account(account))
            .map(|(address, account)| {
                (
                    address,
                    alloy_rlp::encode_fixed_size(&TrieAccount::new(account)),
                )
            }),
    )
}

#[derive(RlpEncodable, RlpMaxEncodedLen)]
struct TrieAccount {
    nonce: u64,
    balance: U256,
    root_hash: B256,
    code_hash: B256,
}

impl TrieAccount {
    fn new(account: &PlainAccount) -> Self {
        Self {
            nonce: account.info.nonce,
            balance: account.info.balance,
            root_hash: sec_trie_root::<KeccakHasher, _, _, _>(
                account
                    .storage
                    .iter()
                    .filter(|(_slot, value)| !value.is_zero())
                    .map(|(slot, value)| {
                        (
                            slot.to_be_bytes::<32>(),
                            alloy_rlp::encode_fixed_size(value),
                        )
                    }),
            ),
            code_hash: account.info.code_hash,
        }
    }
}

fn is_empty_plain_account(account: &PlainAccount) -> bool {
    account.info.is_empty() && account.storage.values().all(|value| value.is_zero())
}

#[inline]
fn trie_root<I, A, B>(input: I) -> B256
where
    I: IntoIterator<Item = (A, B)>,
    A: AsRef<[u8]>,
    B: AsRef<[u8]>,
{
    sec_trie_root::<KeccakHasher, _, _, _>(input)
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Hash)]
struct KeccakHasher;

impl Hasher for KeccakHasher {
    type Out = B256;
    type StdHasher = PlainHasher;
    const LENGTH: usize = 32;

    fn hash(x: &[u8]) -> Self::Out {
        keccak256(x)
    }
}

pub fn compute_state_root_from_fixture_accounts(
    accounts: &alloy_primitives::map::HashMap<Address, FixtureAccountInfo>,
) -> B256 {
    state_root_unhashed(accounts.iter().map(|(address, account)| {
        let storage_root = if account.storage.is_empty() {
            EMPTY_ROOT_HASH
        } else {
            storage_root_unhashed(
                account
                    .storage
                    .iter()
                    .map(|(slot, value)| (B256::from(slot.to_be_bytes::<32>()), *value)),
            )
        };
        (
            *address,
            FixtureTrieAccount {
                nonce: account.nonce,
                balance: account.balance,
                storage_root,
                code_hash: keccak256(&account.code),
            },
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Bytes};
    use revm::state::AccountInfo;
    use revm_database::states::CacheState;

    #[test]
    fn logs_hash_changes_when_log_payload_changes() {
        let baseline_log = Log {
            address: address!("1000000000000000000000000000000000000001"),
            data: revm_primitives::LogData::new_unchecked(
                vec![B256::ZERO],
                Bytes::from(vec![1, 2]),
            ),
        };
        let changed_log = Log {
            address: baseline_log.address,
            data: revm_primitives::LogData::new_unchecked(
                vec![B256::repeat_byte(0x11)],
                Bytes::from(vec![1, 2, 3]),
            ),
        };

        let baseline_hash = compute_logs_hash(std::slice::from_ref(&baseline_log));
        assert_eq!(
            baseline_hash,
            compute_logs_hash(std::slice::from_ref(&baseline_log))
        );
        assert_ne!(
            baseline_hash,
            compute_logs_hash(std::slice::from_ref(&changed_log))
        );
    }

    #[test]
    fn filter_validation_logs_excludes_arc_authority_log() {
        let authority_log = Log {
            address: ARC_NATIVE_COIN_AUTHORITY,
            data: revm_primitives::LogData::new_unchecked(vec![B256::ZERO], Bytes::from(vec![1])),
        };
        let user_log = Log {
            address: address!("1000000000000000000000000000000000000001"),
            data: revm_primitives::LogData::new_unchecked(
                vec![B256::repeat_byte(0x11)],
                Bytes::from(vec![2, 3]),
            ),
        };

        assert_eq!(
            filter_validation_logs(&[authority_log, user_log.clone()]),
            vec![user_log]
        );
    }

    #[test]
    fn revm_state_root_ignores_zero_storage_slots() {
        let address = address!("2000000000000000000000000000000000000002");
        let mut cache = CacheState::new(true);
        cache.insert_account_with_storage(
            address,
            AccountInfo::default(),
            std::collections::HashMap::from_iter([
                (U256::from(1), U256::ZERO),
                (U256::from(2), U256::from(22)),
            ]),
        );

        let root = state_merkle_trie_root(cache.trie_account());

        let expected =
            compute_state_root_from_fixture_accounts(&alloy_primitives::map::HashMap::from_iter([
                (
                    address,
                    FixtureAccountInfo {
                        balance: U256::ZERO,
                        code: Bytes::default(),
                        nonce: 0,
                        storage: alloy_primitives::map::HashMap::from_iter([(
                            U256::from(2),
                            U256::from(22),
                        )]),
                    },
                ),
            ]));

        assert_eq!(root, expected);
    }

    #[test]
    fn state_root_ignores_empty_accounts() {
        let address = address!("3000000000000000000000000000000000000003");
        let mut cache = CacheState::new(true);
        cache.insert_account(address, AccountInfo::default());

        assert_eq!(
            state_merkle_trie_root(cache.trie_account()),
            EMPTY_ROOT_HASH
        );
    }

    #[test]
    fn fixture_state_root_matches_expected_fixture_accounts() {
        let mut accounts = alloy_primitives::map::HashMap::default();
        accounts.insert(
            address!("1000000000000000000000000000000000000001"),
            FixtureAccountInfo {
                balance: U256::from(7),
                code: Bytes::default(),
                nonce: 2,
                storage: alloy_primitives::map::HashMap::from_iter([
                    (U256::from(1), U256::from(11)),
                    (U256::from(2), U256::from(22)),
                ]),
            },
        );

        let root = compute_state_root_from_fixture_accounts(&accounts);
        assert_ne!(root, B256::ZERO);
    }
}
