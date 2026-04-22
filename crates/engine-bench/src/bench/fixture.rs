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

use super::helpers::fmt_hash;
use alloy_genesis::Genesis;
use alloy_primitives::B256;
use alloy_rpc_types_engine::ExecutionPayloadV3;
use eyre::{bail, Context};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

pub(crate) const GENESIS_FILE_NAME: &str = "genesis.json";
pub(crate) const METADATA_FILE_NAME: &str = "metadata.json";
pub(crate) const PAYLOADS_FILE_NAME: &str = "payloads.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExpectedParentBlock {
    pub block_number: u64,
    pub block_hash: B256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PayloadFixtureMetadata {
    pub from_block: u64,
    pub to_block: u64,
    pub payload_count: u64,
    pub expected_parent: ExpectedParentBlock,
}

impl PayloadFixtureMetadata {
    pub(crate) fn validate(&self) -> eyre::Result<()> {
        if self.from_block == 0 {
            bail!("payload fixture from_block must be greater than 0");
        }
        if self.from_block > self.to_block {
            bail!("payload fixture from_block must be less than or equal to to_block");
        }

        let expected_count = self.to_block - self.from_block + 1;
        if self.payload_count != expected_count {
            bail!(
                "payload fixture payload_count mismatch: expected {expected_count}, got {}",
                self.payload_count
            );
        }

        let expected_parent_block_number = self.from_block - 1;
        if self.expected_parent.block_number != expected_parent_block_number {
            bail!(
                "payload fixture expected_parent block number mismatch: expected {expected_parent_block_number}, got {}",
                self.expected_parent.block_number
            );
        }

        Ok(())
    }
}

pub(crate) struct PayloadFixtureWriter {
    output_dir: PathBuf,
    genesis_tmp_path: PathBuf,
    metadata_tmp_path: PathBuf,
    payloads_tmp_path: PathBuf,
    payloads_writer: BufWriter<File>,
}

impl PayloadFixtureWriter {
    pub(crate) fn new(output_dir: &Path) -> eyre::Result<Self> {
        fs::create_dir_all(output_dir).wrap_err_with(|| {
            format!(
                "failed to create payload fixture directory {}",
                output_dir.display()
            )
        })?;

        let genesis_tmp_path = output_dir.join(format!("{GENESIS_FILE_NAME}.tmp"));
        let metadata_tmp_path = output_dir.join(format!("{METADATA_FILE_NAME}.tmp"));
        let payloads_tmp_path = output_dir.join(format!("{PAYLOADS_FILE_NAME}.tmp"));
        remove_file_if_exists(&genesis_tmp_path)?;
        remove_file_if_exists(&metadata_tmp_path)?;
        remove_file_if_exists(&payloads_tmp_path)?;

        let payloads_writer = BufWriter::new(
            File::create(&payloads_tmp_path)
                .wrap_err_with(|| format!("failed to create {}", payloads_tmp_path.display()))?,
        );

        Ok(Self {
            output_dir: output_dir.to_path_buf(),
            genesis_tmp_path,
            metadata_tmp_path,
            payloads_tmp_path,
            payloads_writer,
        })
    }

    pub(crate) fn write_payload(&mut self, payload: &ExecutionPayloadV3) -> eyre::Result<()> {
        serde_json::to_writer(&mut self.payloads_writer, payload)
            .wrap_err("failed to serialize payload fixture entry")?;
        self.payloads_writer
            .write_all(b"\n")
            .wrap_err("failed to write payload fixture newline")?;
        Ok(())
    }

    pub(crate) fn finish(
        self,
        metadata: &PayloadFixtureMetadata,
        genesis: &Genesis,
    ) -> eyre::Result<()> {
        metadata.validate()?;

        let Self {
            output_dir,
            genesis_tmp_path,
            metadata_tmp_path,
            payloads_tmp_path,
            mut payloads_writer,
        } = self;

        payloads_writer
            .flush()
            .wrap_err("failed to flush payload fixture stream")?;
        drop(payloads_writer);

        write_json_file(&genesis_tmp_path, genesis, "genesis")?;
        write_json_file(&metadata_tmp_path, metadata, "metadata")?;

        let genesis_path = genesis_path(&output_dir);
        let metadata_path = metadata_path(&output_dir);
        let payloads_path = payloads_path(&output_dir);
        remove_file_if_exists(&genesis_path)?;
        remove_file_if_exists(&metadata_path)?;
        remove_file_if_exists(&payloads_path)?;
        rename_into_place(&payloads_tmp_path, &payloads_path)?;
        rename_into_place(&genesis_tmp_path, &genesis_path)?;
        rename_into_place(&metadata_tmp_path, &metadata_path)?;

        Ok(())
    }
}

pub(crate) struct PayloadFixture {
    #[allow(dead_code)]
    genesis: Genesis,
    metadata: PayloadFixtureMetadata,
    payload_reader: PayloadJsonlReader,
    expected_next_block: u64,
    expected_parent_hash: B256,
    yielded: u64,
    exhausted: bool,
}

impl PayloadFixture {
    pub(crate) fn open(payload_dir: &Path) -> eyre::Result<Self> {
        let genesis = load_genesis(payload_dir)?;
        let metadata = load_metadata(payload_dir)?;
        let payloads_path = payloads_path(payload_dir);
        if !payloads_path.is_file() {
            bail!(
                "payload fixture payloads file does not exist: {}",
                payloads_path.display()
            );
        }

        Ok(Self {
            expected_next_block: metadata.from_block,
            expected_parent_hash: metadata.expected_parent.block_hash,
            payload_reader: PayloadJsonlReader::new(&payloads_path)?,
            genesis,
            metadata,
            yielded: 0,
            exhausted: false,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn genesis(&self) -> &Genesis {
        &self.genesis
    }

    pub(crate) fn metadata(&self) -> &PayloadFixtureMetadata {
        &self.metadata
    }

    pub(crate) fn next_payload(&mut self) -> eyre::Result<Option<ExecutionPayloadV3>> {
        if self.exhausted {
            return Ok(None);
        }

        if self.yielded == self.metadata.payload_count {
            if let Some(extra_payload) = self.payload_reader.next_payload()? {
                let extra_block = extra_payload.payload_inner.payload_inner.block_number;
                bail!(
                    "payload fixture contains more payloads than expected: first extra block is {extra_block}"
                );
            }
            self.exhausted = true;
            return Ok(None);
        }

        let payload = self.payload_reader.next_payload()?.ok_or_else(|| {
            eyre::eyre!(
                "payload fixture ended early after {} payloads; expected {}",
                self.yielded,
                self.metadata.payload_count
            )
        })?;

        let block = &payload.payload_inner.payload_inner;
        if block.block_number != self.expected_next_block {
            bail!(
                "payload fixture block sequence mismatch: expected block {}, got {}",
                self.expected_next_block,
                block.block_number
            );
        }
        if block.parent_hash != self.expected_parent_hash {
            bail!(
                "payload fixture parent hash mismatch for block {}: expected parent {}, got {}",
                block.block_number,
                fmt_hash(self.expected_parent_hash),
                fmt_hash(block.parent_hash),
            );
        }

        self.yielded = self.yielded.saturating_add(1);
        self.expected_next_block = self
            .expected_next_block
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("payload fixture block number overflow"))?;
        self.expected_parent_hash = block.block_hash;

        Ok(Some(payload))
    }
}

pub(crate) fn genesis_path(payload_dir: &Path) -> PathBuf {
    payload_dir.join(GENESIS_FILE_NAME)
}

pub(crate) fn metadata_path(payload_dir: &Path) -> PathBuf {
    payload_dir.join(METADATA_FILE_NAME)
}

pub(crate) fn payloads_path(payload_dir: &Path) -> PathBuf {
    payload_dir.join(PAYLOADS_FILE_NAME)
}

fn load_genesis(payload_dir: &Path) -> eyre::Result<Genesis> {
    let path = genesis_path(payload_dir);
    load_json_file(&path)
}

fn load_metadata(payload_dir: &Path) -> eyre::Result<PayloadFixtureMetadata> {
    if !payload_dir.is_dir() {
        bail!(
            "payload fixture directory does not exist: {}",
            payload_dir.display()
        );
    }
    let path = metadata_path(payload_dir);
    let metadata: PayloadFixtureMetadata = load_json_file(&path)?;
    metadata.validate()?;
    Ok(metadata)
}

fn load_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> eyre::Result<T> {
    if !path.is_file() {
        bail!("payload fixture file does not exist: {}", path.display());
    }
    let reader = BufReader::new(
        File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?,
    );
    serde_json::from_reader(reader).wrap_err_with(|| format!("failed to parse {}", path.display()))
}

fn write_json_file<T: Serialize>(path: &Path, value: &T, label: &str) -> eyre::Result<()> {
    let mut writer = BufWriter::new(
        File::create(path).wrap_err_with(|| format!("failed to create {}", path.display()))?,
    );
    serde_json::to_writer_pretty(&mut writer, value)
        .wrap_err_with(|| format!("failed to serialize payload fixture {label}"))?;
    writer
        .write_all(b"\n")
        .wrap_err_with(|| format!("failed to write payload fixture {label} newline"))?;
    writer
        .flush()
        .wrap_err_with(|| format!("failed to flush payload fixture {label}"))?;
    Ok(())
}

fn rename_into_place(from: &Path, to: &Path) -> eyre::Result<()> {
    fs::rename(from, to).wrap_err_with(|| {
        format!(
            "failed to move payload fixture file into place: {} -> {}",
            from.display(),
            to.display()
        )
    })
}

fn remove_file_if_exists(path: &Path) -> eyre::Result<()> {
    if path.exists() {
        fs::remove_file(path).wrap_err_with(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

struct PayloadJsonlReader {
    path: PathBuf,
    reader: BufReader<File>,
    line_number: usize,
}

impl PayloadJsonlReader {
    fn new(path: &Path) -> eyre::Result<Self> {
        let file =
            File::open(path).wrap_err_with(|| format!("failed to open {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            reader: BufReader::new(file),
            line_number: 0,
        })
    }

    fn next_payload(&mut self) -> eyre::Result<Option<ExecutionPayloadV3>> {
        let mut line = String::new();
        let bytes_read = self
            .reader
            .read_line(&mut line)
            .wrap_err_with(|| format!("failed to read {}", self.path.display()))?;
        if bytes_read == 0 {
            return Ok(None);
        }

        self.line_number += 1;
        let line = line.trim_end_matches(&['\r', '\n'][..]);
        if line.is_empty() {
            bail!(
                "payload fixture contains an empty line at {}:{}",
                self.path.display(),
                self.line_number
            );
        }

        let payload = serde_json::from_str(line).wrap_err_with(|| {
            format!(
                "failed to parse payload fixture entry at {}:{}",
                self.path.display(),
                self.line_number
            )
        })?;
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_genesis() -> Genesis {
        Genesis::default()
    }

    fn zero_address() -> String {
        format!("0x{}", "00".repeat(20))
    }

    fn zero_bloom() -> String {
        format!("0x{}", "00".repeat(256))
    }

    fn hash_hex(value: u64) -> String {
        format!("0x{value:064x}")
    }

    fn payload(block_number: u64, parent_hash: u64, block_hash: u64) -> ExecutionPayloadV3 {
        serde_json::from_value(json!({
            "parentHash": hash_hex(parent_hash),
            "feeRecipient": zero_address(),
            "stateRoot": hash_hex(1_000 + block_number),
            "receiptsRoot": hash_hex(2_000 + block_number),
            "logsBloom": zero_bloom(),
            "prevRandao": hash_hex(3_000 + block_number),
            "blockNumber": format!("0x{block_number:x}"),
            "gasLimit": "0x1c9c380",
            "gasUsed": format!("0x{:x}", block_number * 1_000),
            "timestamp": format!("0x{:x}", 10_000 + block_number),
            "extraData": "0x",
            "baseFeePerGas": "0x1",
            "blockHash": hash_hex(block_hash),
            "transactions": [],
            "withdrawals": [],
            "blobGasUsed": "0x0",
            "excessBlobGas": "0x0"
        }))
        .unwrap()
    }

    fn metadata_for(
        first_payload: &ExecutionPayloadV3,
        to_block: u64,
        payload_count: u64,
    ) -> PayloadFixtureMetadata {
        PayloadFixtureMetadata {
            from_block: first_payload.payload_inner.payload_inner.block_number,
            to_block,
            payload_count,
            expected_parent: ExpectedParentBlock {
                block_number: first_payload.payload_inner.payload_inner.block_number - 1,
                block_hash: first_payload.payload_inner.payload_inner.parent_hash,
            },
        }
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let first_payload = payload(5, 44, 55);
        let metadata = metadata_for(&first_payload, 6, 2);

        let encoded = serde_json::to_string(&metadata).unwrap();
        let decoded: PayloadFixtureMetadata = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, metadata);
    }

    #[test]
    fn metadata_validation_rejects_count_mismatch() {
        let first_payload = payload(5, 44, 55);
        let mut metadata = metadata_for(&first_payload, 6, 2);
        metadata.payload_count = 3;

        let err = metadata.validate().unwrap_err();

        assert_eq!(
            err.to_string(),
            "payload fixture payload_count mismatch: expected 2, got 3"
        );
    }

    #[test]
    fn payload_fixture_writer_and_reader_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let payload1 = payload(7, 66, 77);
        let payload2 = payload(8, 77, 88);
        let metadata = metadata_for(&payload1, 8, 2);

        let mut writer = PayloadFixtureWriter::new(temp_dir.path()).unwrap();
        writer.write_payload(&payload1).unwrap();
        writer.write_payload(&payload2).unwrap();
        writer.finish(&metadata, &test_genesis()).unwrap();

        let mut fixture = PayloadFixture::open(temp_dir.path()).unwrap();

        assert_eq!(fixture.metadata(), &metadata);
        assert_eq!(fixture.next_payload().unwrap(), Some(payload1));
        assert_eq!(fixture.next_payload().unwrap(), Some(payload2));
        assert_eq!(fixture.next_payload().unwrap(), None);
    }

    #[test]
    fn payload_fixture_open_fails_when_files_are_missing() {
        let temp_dir = TempDir::new().unwrap();

        let err = match PayloadFixture::open(temp_dir.path()) {
            Ok(_) => panic!("expected missing fixture files to fail"),
            Err(err) => err,
        };

        assert_eq!(
            err.to_string(),
            format!(
                "payload fixture file does not exist: {}",
                temp_dir.path().join(GENESIS_FILE_NAME).display()
            )
        );
    }

    #[test]
    fn payload_fixture_rejects_block_gaps() {
        let temp_dir = TempDir::new().unwrap();
        let payload1 = payload(10, 99, 100);
        let payload3 = payload(12, 100, 101);
        let metadata = metadata_for(&payload1, 11, 2);

        let mut writer = PayloadFixtureWriter::new(temp_dir.path()).unwrap();
        writer.write_payload(&payload1).unwrap();
        writer.write_payload(&payload3).unwrap();
        writer.finish(&metadata, &test_genesis()).unwrap();

        let mut fixture = PayloadFixture::open(temp_dir.path()).unwrap();
        assert_eq!(fixture.next_payload().unwrap(), Some(payload1));

        let err = fixture.next_payload().unwrap_err();
        assert_eq!(
            err.to_string(),
            "payload fixture block sequence mismatch: expected block 11, got 12"
        );
    }

    #[test]
    fn payload_fixture_rejects_extra_payloads() {
        let temp_dir = TempDir::new().unwrap();
        let payload1 = payload(20, 199, 200);
        let payload2 = payload(21, 200, 201);
        let metadata = metadata_for(&payload1, 20, 1);

        let mut writer = PayloadFixtureWriter::new(temp_dir.path()).unwrap();
        writer.write_payload(&payload1).unwrap();
        writer.write_payload(&payload2).unwrap();
        writer.finish(&metadata, &test_genesis()).unwrap();

        let mut fixture = PayloadFixture::open(temp_dir.path()).unwrap();
        assert_eq!(fixture.next_payload().unwrap(), Some(payload1));

        let err = fixture.next_payload().unwrap_err();
        assert_eq!(
            err.to_string(),
            "payload fixture contains more payloads than expected: first extra block is 21"
        );
    }
}
