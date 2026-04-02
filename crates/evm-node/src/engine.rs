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

//! Arc validator
//! fork from https://github.com/paradigmxyz/reth/blob/main/crates/ethereum/node/src/engine.rs
//! - customize validate_payload_attributes_against_header to relax the timestamp constraint.

use alloy_consensus::BlockHeader;
use alloy_rpc_types_engine::ExecutionData;
pub use alloy_rpc_types_engine::{
    ExecutionPayloadEnvelopeV2, ExecutionPayloadEnvelopeV3, ExecutionPayloadEnvelopeV4,
    ExecutionPayloadV1,
};
use reth_chainspec::{EthChainSpec, EthereumHardforks};
use reth_engine_primitives::{EngineApiValidator, PayloadValidator};
use reth_ethereum_engine_primitives::EthPayloadAttributes;
use reth_ethereum_payload_builder::EthereumExecutionPayloadValidator;
use reth_ethereum_primitives::Block;
use reth_node_api::PayloadTypes;
use reth_payload_primitives::InvalidPayloadAttributesError;
use reth_payload_primitives::PayloadAttributes;
use reth_payload_primitives::{
    validate_execution_requests, validate_version_specific_fields, EngineApiMessageVersion,
    EngineObjectValidationError, NewPayloadError, PayloadOrAttributes,
};
use reth_primitives_traits::Block as BlockTr;
use reth_primitives_traits::RecoveredBlock;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ArcEngineValidator<ChainSpec = reth_chainspec::ChainSpec> {
    inner: EthereumExecutionPayloadValidator<ChainSpec>,
}

impl<ChainSpec> ArcEngineValidator<ChainSpec> {
    /// Instantiates a new validator.
    pub const fn new(chain_spec: Arc<ChainSpec>) -> Self {
        Self {
            inner: EthereumExecutionPayloadValidator::new(chain_spec),
        }
    }

    /// Returns the chain spec used by the validator.
    #[inline]
    fn chain_spec(&self) -> &ChainSpec {
        self.inner.chain_spec()
    }
}

/// Type that validates an [`ExecutionPayload`].
impl<ChainSpec, Types> PayloadValidator<Types> for ArcEngineValidator<ChainSpec>
where
    ChainSpec: EthChainSpec + EthereumHardforks + 'static,
    Types: PayloadTypes<ExecutionData = ExecutionData>,
    Types::PayloadAttributes: PayloadAttributes,
{
    type Block = Block;

    fn convert_payload_to_block(
        &self,
        payload: ExecutionData,
    ) -> Result<reth_primitives_traits::SealedBlock<Self::Block>, NewPayloadError> {
        if payload
            .sidecar
            .versioned_hashes()
            .is_some_and(|v| !v.is_empty())
            || payload.payload.blob_gas_used().is_some_and(|g| g > 0)
        {
            return Err(NewPayloadError::Other(
                "Blob transactions are not supported".into(),
            ));
        }

        self.inner
            .ensure_well_formed_payload(payload)
            .map_err(Into::into)
    }

    fn ensure_well_formed_payload(
        &self,
        payload: ExecutionData,
    ) -> Result<RecoveredBlock<Self::Block>, NewPayloadError> {
        let sealed_block =
            <Self as PayloadValidator<Types>>::convert_payload_to_block(self, payload)?;
        sealed_block
            .try_recover()
            .map_err(|e| NewPayloadError::Other(e.into()))
    }

    fn validate_payload_attributes_against_header(
        &self,
        attr: &Types::PayloadAttributes,
        header: &<Self::Block as BlockTr>::Header,
    ) -> Result<(), InvalidPayloadAttributesError> {
        // NOTE(romac): Here we relax the check to allow the payload attributes timestamp
        //              to be greater than or equal to the header's timestamp.
        if attr.timestamp() < header.timestamp() {
            return Err(InvalidPayloadAttributesError::InvalidTimestamp);
        }
        Ok(())
    }
}

/// Type that validates the payloads processed by the engine.
impl<ChainSpec, Types> EngineApiValidator<Types> for ArcEngineValidator<ChainSpec>
where
    ChainSpec: EthChainSpec + EthereumHardforks + 'static,
    Types: PayloadTypes<PayloadAttributes = EthPayloadAttributes, ExecutionData = ExecutionData>,
{
    fn validate_version_specific_fields(
        &self,
        version: EngineApiMessageVersion,
        payload_or_attrs: PayloadOrAttributes<'_, Types::ExecutionData, EthPayloadAttributes>,
    ) -> Result<(), EngineObjectValidationError> {
        payload_or_attrs
            .execution_requests()
            .map(|requests| validate_execution_requests(requests))
            .transpose()?;

        validate_version_specific_fields(self.chain_spec(), version, payload_or_attrs)
    }

    fn ensure_well_formed_attributes(
        &self,
        version: EngineApiMessageVersion,
        attributes: &EthPayloadAttributes,
    ) -> Result<(), EngineObjectValidationError> {
        validate_version_specific_fields(
            self.chain_spec(),
            version,
            PayloadOrAttributes::<Types::ExecutionData, EthPayloadAttributes>::PayloadAttributes(
                attributes,
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{hex, Address, Bytes, B256, U256};
    use alloy_rpc_types_engine::{
        CancunPayloadFields, ExecutionPayload, ExecutionPayloadSidecar, ExecutionPayloadV1,
        ExecutionPayloadV2, ExecutionPayloadV3, PraguePayloadFields,
    };
    use reth_chainspec::ChainSpecBuilder;
    use reth_ethereum::primitives::Header;
    use reth_ethereum_engine_primitives::{EthEngineTypes, EthPayloadTypes};
    use revm_primitives::hex::FromHex;

    fn create_validator() -> ArcEngineValidator {
        let chain_spec = Arc::new(ChainSpecBuilder::mainnet().build());
        ArcEngineValidator::new(chain_spec)
    }

    fn create_test_payload_v3(blob_gas_used: u64) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    base_fee_per_gas:  U256::from(7u64),
                    block_number: 0xa946u64,
                    block_hash: hex!("a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b").into(),
                    logs_bloom: hex!("00200004000000000000000080000000000200000000000000000000000000000000200000000000000000000000000000000000800000000200000000000000000000000000000000000008000000200000000000000000000001000000000000000000000000000000800000000000000000000100000000000030000000000000000040000000000000000000000000000000000800080080404000000000000008000000000008200000000000200000000000000000000000000000000000000002000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000100000000000000000000").into(),
                    extra_data: hex!("d883010d03846765746888676f312e32312e31856c696e7578").into(),
                    gas_limit: 0x1c9c380,
                    gas_used: 0x1f4a9,
                    timestamp: 0x651f35b8,
                    fee_recipient: hex!("f97e180c050e5ab072211ad2c213eb5aee4df134").into(),
                    parent_hash: hex!("a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b").into(),
                    prev_randao: hex!("a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b").into(),
                    receipts_root: hex!("a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b").into(),
                    state_root: hex!("a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b").into(),
                    transactions: vec![Bytes::from_hex("0x1234").unwrap()],
                },
                withdrawals: vec![],
            },
            blob_gas_used,
            excess_blob_gas: 0x0,
        }
    }

    #[test]
    fn validate_payload_attributes_against_header_valid() {
        let validator = create_validator();

        let parent_header = Header {
            timestamp: 1000,
            ..Default::default()
        };

        // Equal timestamp
        let attributes_eq = EthPayloadAttributes {
            timestamp: 1000,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: None,
            parent_beacon_block_root: None,
        };

        assert!(<ArcEngineValidator<_> as PayloadValidator<EthEngineTypes>>::validate_payload_attributes_against_header(
            &validator,
            &attributes_eq,
            &parent_header,
        ).is_ok());

        // Greater timestamp
        let attributes_gt = EthPayloadAttributes {
            timestamp: 1001,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: None,
            parent_beacon_block_root: None,
        };

        assert!(<ArcEngineValidator<_> as PayloadValidator<EthEngineTypes>>::validate_payload_attributes_against_header(
            &validator,
            &attributes_gt,
            &parent_header,
        ).is_ok());
    }

    #[test]
    fn validate_payload_attributes_against_header_invalid() {
        let validator = create_validator();

        let parent_header = Header {
            timestamp: 1000,
            ..Default::default()
        };

        let attributes = EthPayloadAttributes {
            timestamp: 999,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            withdrawals: None,
            parent_beacon_block_root: None,
        };

        assert!(matches!(<ArcEngineValidator<_> as PayloadValidator<EthEngineTypes>>::validate_payload_attributes_against_header(
            &validator,
            &attributes,
            &parent_header,
        ), Err(InvalidPayloadAttributesError::InvalidTimestamp)));
    }

    #[test]
    fn ensure_well_formed_payload_rejects_blob_versioned_hashes() {
        let validator = create_validator();
        let new_payload = create_test_payload_v3(0);

        let result =
            <ArcEngineValidator as PayloadValidator<EthPayloadTypes>>::ensure_well_formed_payload(
                &validator,
                ExecutionData {
                    payload: ExecutionPayload::V3(new_payload),
                    sidecar: ExecutionPayloadSidecar::v4(
                        CancunPayloadFields::new(
                            hex!(
                                "a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b"
                            )
                            .into(),
                            vec![B256::from(hex!(
                                "a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b"
                            ))],
                        ),
                        PraguePayloadFields::default(),
                    ),
                },
            );

        match result {
            Err(NewPayloadError::Other(msg)) => {
                assert_eq!(msg.to_string(), "Blob transactions are not supported");
            }
            _ => panic!("Unexpected result: {:?}", result),
        }
    }

    #[test]
    fn ensure_well_formed_payload_rejects_blob_gas_used() {
        let validator = create_validator();
        let new_payload = create_test_payload_v3(10);

        let result =
            <ArcEngineValidator as PayloadValidator<EthPayloadTypes>>::ensure_well_formed_payload(
                &validator,
                ExecutionData {
                    payload: ExecutionPayload::V3(new_payload),
                    sidecar: ExecutionPayloadSidecar::v4(
                        CancunPayloadFields::new(
                            hex!(
                                "a5ddd3f286f429458a39cafc13ffe89295a7efa8eb363cf89a1a4887dbcf272b"
                            )
                            .into(),
                            vec![],
                        ),
                        PraguePayloadFields::default(),
                    ),
                },
            );

        match result {
            Err(NewPayloadError::Other(msg)) => {
                assert_eq!(msg.to_string(), "Blob transactions are not supported");
            }
            _ => panic!("Unexpected result: {:?}", result),
        }
    }
}
