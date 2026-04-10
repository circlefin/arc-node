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

use eyre::{eyre, Context};
use tracing::{error, info};

use malachitebft_app_channel::app::streaming::StreamId;
use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::Channels;

use arc_consensus_types::signing::SigningProvider;
use arc_consensus_types::{ArcContext, BlockHash, Height, ValueId};

use crate::block::ConsensusBlock;
use crate::proposal_parts::{prepare_stream, stream_proposal, PublishProposalPart};
use crate::state::State;
use crate::store::repositories::UndecidedBlocksRepository;

/// Handles the `RestreamProposal` message from the consensus engine.
///
/// This is called when the consensus engine requests to restream a proposal for a specific height and round.
/// The block is looked up by height and block hash (ignoring round), so it will be found
/// regardless of which round it was originally stored under. The block's round and valid_round
/// are updated to match the current proposal context before restreaming.
///
/// ## Errors
/// - If no block is found for the specified height and round
/// - If there are issues fetching or storing the block in the repository
/// - If there are issues preparing or streaming the proposal parts
pub async fn handle(
    state: &mut State,
    channels: &mut Channels<ArcContext>,
    height: Height,
    round: Round,
    valid_round: Round,
    value_id: ValueId,
) -> eyre::Result<()> {
    let block_to_restream = get_block_to_restream(
        state.store(),
        height,
        round,
        valid_round,
        value_id.block_hash(),
    )
    .await?;

    if let Some(block) = block_to_restream {
        let stream_id = state.next_stream_id();
        let signing_provider = state.signing_provider();

        restream_proposal(&channels.network, stream_id, signing_provider, &block).await
    } else {
        error!(%height, %round, %valid_round, "No block found to restream");

        Err(eyre!("No block found to restream (height={height}, round={round}, valid_round={valid_round})"))
    }
}

pub async fn restream_proposal(
    publish: impl PublishProposalPart,
    stream_id: StreamId,
    signing_provider: &impl SigningProvider<ArcContext>,
    block: &ConsensusBlock,
) -> eyre::Result<()> {
    let (height, round) = (block.height, block.round);

    info!(
        %height, %round, valid_round = %block.valid_round,
        "Restreaming proposal, block size: {:?}, payload size: {:?}",
        block.size_bytes(), block.payload_size()
    );

    let (stream_messages, _signature) = prepare_stream(stream_id, signing_provider, block)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to prepare proposal parts for restreaming (height={height}, round={round})"
            )
        })?;

    stream_proposal(publish, height, round, stream_messages)
        .await
        .wrap_err_with(|| {
            format!("Failed to restream proposal (height={height}, round={round})")
        })?;

    Ok(())
}

async fn get_block_to_restream(
    undecided_blocks: impl UndecidedBlocksRepository,
    height: Height,
    round: Round,
    valid_round: Round,
    block_hash: BlockHash,
) -> eyre::Result<Option<ConsensusBlock>> {
    let block = undecided_blocks
        .get_first(height, block_hash)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to fetch block for restreaming \
                 (height={height}, round={round}, block_hash={block_hash})"
            )
        })?;

    if let Some(mut block) = block {
        block.round = round;
        block.valid_round = valid_round;

        undecided_blocks
            .store(block.clone())
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to store updated block before restreaming \
                     (height={height}, round={round}, block_hash={block_hash})"
                )
            })?;

        Ok(Some(block))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use crate::proposal_parts::MockPublishProposalPart;
    use crate::store::repositories::mocks::MockUndecidedBlocksRepository;

    use super::*;

    use alloy_rpc_types_engine::ExecutionPayloadV3;
    use arbitrary::Arbitrary;
    use arc_consensus_types::{Address, BlockHash, Height};
    use arc_signer::local::{LocalSigningProvider, PrivateKey};
    use bytes::Bytes;
    use malachitebft_app_channel::app::types::core::Round;
    use malachitebft_core_types::Validity;
    use mockall::predicate::eq;

    fn create_dummy_block(height: Height, round: Round, valid_round: Round) -> ConsensusBlock {
        let mut u = arbitrary::Unstructured::new(&[0u8; 1024]);

        ConsensusBlock {
            height,
            round,
            valid_round,
            proposer: Address::arbitrary(&mut u).unwrap(),
            validity: Validity::Valid,
            execution_payload: ExecutionPayloadV3::arbitrary(&mut u).unwrap(),
            signature: None,
        }
    }

    #[tokio::test]
    async fn get_block_found_and_updated() {
        let mut mock_repo = MockUndecidedBlocksRepository::new();

        let height = Height::new(10);
        let round = Round::new(5);
        let valid_round = Round::new(3);
        let block_hash = BlockHash::default();

        let original_block = create_dummy_block(height, Round::new(0), Round::Nil);

        mock_repo
            .expect_get_first()
            .with(eq(height), eq(block_hash))
            .times(1)
            .returning(move |_, _| Ok(Some(original_block.clone())));

        mock_repo
            .expect_store()
            .withf(move |b| b.round == round && b.valid_round == valid_round)
            .times(1)
            .returning(|_| Ok(()));

        let result =
            get_block_to_restream(&mock_repo, height, round, valid_round, block_hash).await;

        let block = result.unwrap().expect("block should be found");
        assert_eq!(block.round, round);
        assert_eq!(block.valid_round, valid_round);
    }

    #[tokio::test]
    async fn get_block_not_found() {
        let mut mock_repo = MockUndecidedBlocksRepository::new();

        let height = Height::new(10);
        let round = Round::new(5);
        let valid_round = Round::new(3);
        let block_hash = BlockHash::default();

        mock_repo
            .expect_get_first()
            .with(eq(height), eq(block_hash))
            .times(1)
            .returning(|_, _| Ok(None));

        let result =
            get_block_to_restream(&mock_repo, height, round, valid_round, block_hash).await;

        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_block_repo_error_propagation() {
        let mut mock_repo = MockUndecidedBlocksRepository::new();
        let height = Height::new(10);
        let round = Round::new(5);
        let valid_round = Round::Nil;

        mock_repo
            .expect_get_first()
            .returning(|_, _| Err(std::io::Error::other("DB connection failed")));

        let result =
            get_block_to_restream(&mock_repo, height, round, valid_round, BlockHash::default())
                .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to fetch block"));
    }

    #[tokio::test]
    async fn restream_proposal_happy_path() {
        let mut rng = rand::thread_rng();

        let mut mock = MockPublishProposalPart::new();
        let stream_id = StreamId::new(Bytes::from_static(&[42; 20]));
        let signing_provider = LocalSigningProvider::new(PrivateKey::generate(&mut rng));
        let block = create_dummy_block(Height::new(10), Round::new(2), Round::Nil);

        mock.expect_publish_proposal_part().returning(|_| Ok(()));

        let result = restream_proposal(mock, stream_id, &signing_provider, &block).await;

        assert!(result.is_ok());
    }
}
