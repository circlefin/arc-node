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

use axum::extract::FromRef;
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use itertools::Itertools;
use serde::Serialize;
use serde_json::json;
use serde_with::{base64::Base64, serde_as};
use std::collections::BTreeMap;
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc::Sender;
use tracing::error;

use arc_consensus_types::{
    evidence::{DoubleProposal, DoubleVote, StoredMisbehaviorEvidence, ValidatorEvidence},
    signing::PublicKey,
    Address, ArcContext, BlockHash, Height, Validator, ValidatorSet, Value, ValueId,
};
use malachitebft_app_channel::app::engine::{
    consensus::state_dump::{types::VotePerRound, StateDump, VoteKeeperState},
    network::NetworkStateDump,
};
use malachitebft_app_channel::app::net::{Multiaddr, PeerId};
use malachitebft_app_channel::ConsensusRequest;
use malachitebft_app_channel::ConsensusRequestError;
use malachitebft_app_channel::NetworkRequest;
use malachitebft_core_state_machine::state::{RoundValue, State as MState};
use malachitebft_core_types::{CommitSignature, NilOrVal, VoteType};
use malachitebft_network::PersistentPeerError;

use crate::request::{AppRequestError, CommitCertificateInfo, Status, TxAppReq};
use alloy_rpc_types_engine::ExecutionPayloadV3;

use arc_consensus_db::invalid_payloads::{InvalidPayload, StoredInvalidPayloads};
use arc_consensus_types::proposal_monitor::ProposalMonitor;

pub(crate) type TxConsensusReq = Sender<ConsensusRequest<ArcContext>>;
pub(crate) type TxNetworkReq = Sender<NetworkRequest>;

#[derive(Clone, FromRef)]
pub(crate) struct RpcState {
    pub tx_consensus_req: TxConsensusReq,
    pub tx_app_req: TxAppReq,
    pub tx_network_req: TxNetworkReq,
}

pub(crate) struct RouteDef {
    pub method: &'static str,
    pub path: &'static str,
    pub handler: fn() -> axum::routing::MethodRouter<RpcState>,
    pub doc: EndpointInfo,
}

macro_rules! method_str {
    (get) => {
        "GET"
    };
    (post) => {
        "POST"
    };
    (delete) => {
        "DELETE"
    };
}

macro_rules! routes {
    ( $( $route_def:expr ),* $(,)? ) => {
        fn build_routes() -> Vec<crate::rpc::types::RouteDef> {
            vec![ $( $route_def ),* ]
        }
    };
}

macro_rules! route {
    ($method:ident, $path:expr, $handler_fn:path, $desc:expr) => {
        route!(@build $method, $path, $handler_fn, $desc, None)
    };
    ($method:ident, $path:expr, $handler_fn:path, $desc:expr, params = { $( $pkey:expr => $pval:expr ),* $(,)? }) => {
        route!(@build $method, $path, $handler_fn, $desc, Some(::std::collections::BTreeMap::from([ $( ($pkey, $pval) ),* ])))
    };
    (@build $method:ident, $path:expr, $handler_fn:path, $desc:expr, $params:expr) => {
        crate::rpc::types::RouteDef {
            method: method_str!($method),
            path: $path,
            handler: || axum::routing::$method($handler_fn),
            doc: crate::rpc::types::EndpointInfo {
                desc: $desc,
                params: $params,
            },
        }
    };
}

#[derive(serde::Serialize)]
pub(crate) struct EndpointInfo {
    pub desc: &'static str,
    pub params: Option<BTreeMap<&'static str, &'static str>>,
}

#[derive(Serialize)]
pub(crate) struct RpcVersion {
    pub git_version: &'static str,
    pub git_commit: &'static str,
    pub git_short_hash: &'static str,
    pub cargo_version: &'static str,
}

#[derive(Serialize)]
pub(crate) struct RpcNetworkStateDump {
    local_node: RpcLocalNodeInfo,
    peers: Vec<RpcPeerInfo>,
    persistent_peer_ids: Vec<PeerId>,
    persistent_peer_addrs: Vec<Multiaddr>,
    validator_set: RpcNwValidatorSet,
}

#[derive(Serialize)]
struct RpcLocalNodeInfo {
    moniker: String,
    peer_id: PeerId,
    listen_addr: Multiaddr,
    consensus_address: Option<String>,
    is_validator: bool,
    persistent_peers_only: bool,
    subscribed_topics: Vec<String>,
}

#[derive(Serialize)]
struct RpcPeerInfo {
    peer_id: PeerId,
    p2p_address: Multiaddr,
    consensus_address: Option<String>,
    moniker: String,
    peer_type: String,
    connection_direction: Option<String>,
    score: f64,
    topics: Vec<String>,
}

impl From<NetworkStateDump> for RpcNetworkStateDump {
    fn from(s: NetworkStateDump) -> Self {
        let local_node = RpcLocalNodeInfo {
            moniker: s.local_node.moniker,
            peer_id: s.local_node.peer_id,
            listen_addr: s.local_node.listen_addr,
            consensus_address: s.local_node.consensus_address,
            is_validator: s.local_node.is_validator,
            persistent_peers_only: s.local_node.persistent_peers_only,
            subscribed_topics: {
                let mut v = s
                    .local_node
                    .subscribed_topics
                    .into_iter()
                    .collect::<Vec<_>>();
                v.sort();
                v
            },
        };

        let peers = s
            .peers
            .into_iter()
            .map(|(peer_id, info)| RpcPeerInfo {
                peer_id,
                p2p_address: info.address,
                consensus_address: info.consensus_address,
                moniker: info.moniker,
                peer_type: info.peer_type.primary_type_str().to_string(),
                connection_direction: info.connection_direction.map(|d| d.as_str().to_string()),
                score: info.score,
                topics: {
                    let mut t = info.topics.iter().cloned().collect::<Vec<_>>();
                    t.sort();
                    t
                },
            })
            .sorted_by_key(|p| (p.peer_type.clone(), p.moniker.clone()))
            .collect::<Vec<_>>();

        let validator_pairs = s
            .validator_set
            .into_iter()
            .map(|v| (v.address, v.voting_power))
            .collect::<Vec<_>>();
        let validator_set = build_network_validator_set(&validator_pairs);

        RpcNetworkStateDump {
            local_node,
            peers,
            persistent_peer_ids: s.persistent_peer_ids,
            persistent_peer_addrs: s.persistent_peer_addrs,
            validator_set,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcNwValidatorSet {
    total_voting_power: u64,
    count: usize,
    validators: Vec<RpcNwValidatorInfo>,
}

#[derive(Serialize)]
pub(crate) struct RpcNwValidatorInfo {
    address: String,
    voting_power: u64,
}

pub(crate) fn build_network_validator_set(vs: &[(String, u64)]) -> RpcNwValidatorSet {
    let total_voting_power: u64 = vs.iter().map(|(_, vp)| *vp).sum();
    let validators = vs
        .iter()
        .map(|(addr, vp)| RpcNwValidatorInfo {
            address: addr.clone(),
            voting_power: *vp,
        })
        .collect();
    RpcNwValidatorSet {
        total_voting_power,
        count: vs.len(),
        validators,
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GetCertificateParams {
    pub height: Option<Height>,
}

#[derive(serde::Deserialize)]
pub(crate) struct GetProposalMonitorParams {
    pub height: Option<Height>,
}

#[derive(serde::Deserialize)]
pub(crate) struct AddOrRemovePersistentPeerBody {
    pub addr: String,
}

#[derive(Serialize)]
pub(crate) struct RpcProposalMonitorData {
    pub height: u64,
    pub proposer: Address,
    pub start_time: DateTime<Utc>,
    pub proposal_receive_time: Option<DateTime<Utc>>,
    pub value_id: Option<ValueId>,
    pub successful: Option<bool>,
    pub synced: bool,
    pub proposal_delay_ms: Option<i64>,
}

impl From<ProposalMonitor> for RpcProposalMonitorData {
    fn from(data: ProposalMonitor) -> Self {
        let start_time: DateTime<Utc> = data.start_time.into();
        let proposal_receive_time: Option<DateTime<Utc>> =
            data.proposal_receive_time.map(Into::into);

        // It can be negative
        let proposal_delay_ms = data.proposal_receive_time.map(|receive| {
            let start_ms = data
                .start_time
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let receive_ms = receive
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            receive_ms - start_ms
        });

        RpcProposalMonitorData {
            height: data.height.as_u64(),
            proposer: data.proposer,
            start_time,
            proposal_receive_time,
            value_id: data.value_id,
            successful: data.successful.to_bool(),
            synced: data.synced,
            proposal_delay_ms,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcAppStatus {
    height: u64,
    round: i64,
    address: Address,
    proposer: Address,
    height_start_time: DateTime<Utc>,
    prev_payload_hash: Option<BlockHash>,
    db_latest_height: u64,
    db_earliest_height: u64,
    undecided_blocks_count: usize,
    pending_proposal_parts: Vec<RpcPendingProposalParts>,
    validator_set: RpcValidatorSet,
}

impl From<Status> for RpcAppStatus {
    fn from(status: Status) -> Self {
        RpcAppStatus {
            height: status.height.as_u64(),
            round: status.round.as_i64(),
            address: status.address,
            proposer: status.proposer.unwrap_or_else(|| Address::repeat_byte(0)),
            height_start_time: status.height_start_time.into(),
            prev_payload_hash: status.prev_payload_hash,
            db_latest_height: status.db_latest_height.as_u64(),
            db_earliest_height: status.db_earliest_height.as_u64(),
            validator_set: RpcValidatorSet::from(&status.validator_set),
            undecided_blocks_count: status.undecided_blocks_count,
            pending_proposal_parts: status
                .pending_proposal_parts
                .into_iter()
                .map(|(height, count)| RpcPendingProposalParts {
                    height: height.as_u64(),
                    count,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct RpcPendingProposalParts {
    height: u64,
    count: usize,
}

#[serde_as]
#[derive(Serialize)]
struct RpcCommitSignature {
    address: Address,
    #[serde_as(as = "Base64")]
    signature: Vec<u8>,
}

impl From<CommitSignature<ArcContext>> for RpcCommitSignature {
    fn from(sig: CommitSignature<ArcContext>) -> Self {
        RpcCommitSignature {
            address: sig.address,
            signature: sig.signature.to_bytes().to_vec(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcCommitCertificate {
    height: u64,
    round: i64,
    block_hash: ValueId,
    signatures: Vec<RpcCommitSignature>,
    proposer: Address,
    extended: bool,
}

impl From<CommitCertificateInfo> for RpcCommitCertificate {
    fn from(info: CommitCertificateInfo) -> Self {
        RpcCommitCertificate {
            height: info.certificate.height.as_u64(),
            round: info.certificate.round.as_i64(),
            block_hash: info.certificate.value_id,
            signatures: info
                .certificate
                .commit_signatures
                .into_iter()
                .map(Into::into)
                .collect(),
            proposer: info.proposer,
            extended: info.certificate_type.is_extended(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcConsensusStateDump {
    address: Address,
    proposer: Option<Address>,
    validator_set: RpcValidatorSet,
    consensus: RpcConsensus,
    vote_keeper: RpcVoteKeeperState,
}

#[derive(Serialize)]
struct RpcValidatorSet {
    total_voting_power: u64,
    count: usize,
    validators: Vec<RpcValidator>,
}

#[derive(Serialize)]
struct RpcValidator {
    address: Address,
    voting_power: u64,
    public_key: PublicKey,
}

#[derive(Serialize)]
struct RpcConsensus {
    height: u64,
    round: i64,
    step: String,
    locked: Option<RpcRoundValue>,
    valid: Option<RpcRoundValue>,
    decision: Option<RpcRoundValue>,
}

#[derive(Serialize)]
struct RpcRoundVotes {
    round: i64,
    prevotes: VoteData,
    precommits: VoteData,
}

#[derive(Serialize)]
struct RpcVoteKeeperState {
    total_validators: usize,
    rounds: Vec<RpcRoundVotes>,
}

impl RpcVoteKeeperState {
    fn new(vk: &VoteKeeperState<ArcContext>, val_set: &ValidatorSet) -> Self {
        let rounds = vk
            .votes
            .iter()
            .map(|(round, per_round)| {
                let prevotes = VoteData::new(val_set, per_round, VoteType::Prevote);
                let precommits = VoteData::new(val_set, per_round, VoteType::Precommit);

                RpcRoundVotes {
                    round: round.as_i64(),
                    prevotes,
                    precommits,
                }
            })
            .collect();

        Self {
            total_validators: val_set.len(),
            rounds,
        }
    }
}

#[derive(Serialize)]
struct VoteData {
    votes: Vec<RpcVote>,
    #[serde(flatten)]
    details: VotesDetails,
}

#[derive(Serialize)]
struct RpcVote {
    address: Address,
    value: Option<ValueId>,
}

#[derive(Serialize)]
struct VotesDetails {
    total_validators_count: usize,
    total_voting_power: u64,
    validators_count: usize,
    voting_power: u64,
    fraction: f64,
    bit_array: String,
}

impl VotesDetails {
    fn new(val_set: &ValidatorSet, voted: &[RpcVote]) -> Self {
        const X_GROUP: usize = 5;

        let total_validators_count = val_set.len();
        let total_voting_power = val_set.total_voting_power();

        let mut voting_power = 0u64;
        let mut bit_array =
            String::with_capacity(total_validators_count + total_validators_count / X_GROUP);

        for (i, v) in val_set.iter().enumerate() {
            if i > 0 && i % X_GROUP == 0 {
                bit_array.push(' ');
            }
            if voted.iter().any(|d| d.address == v.address) {
                bit_array.push('X');
                voting_power += v.voting_power;
            } else {
                bit_array.push('_');
            }
        }

        let fraction = if total_voting_power > 0 {
            voting_power as f64 / total_voting_power as f64
        } else {
            0.0
        };

        Self {
            total_validators_count,
            total_voting_power,
            validators_count: voted.len(),
            voting_power,
            fraction,
            bit_array,
        }
    }
}

impl VoteData {
    fn new(
        val_set: &ValidatorSet,
        per_round: &VotePerRound<ArcContext>,
        vote_type: VoteType,
    ) -> Self {
        let to_option = |v: NilOrVal<ValueId>| match v {
            NilOrVal::Nil => None,
            NilOrVal::Val(val) => Some(val),
        };

        let votes = per_round
            .received_votes()
            .iter()
            .filter(|sv| sv.typ == vote_type)
            .map(|sv| RpcVote {
                address: sv.validator_address,
                value: to_option(sv.value),
            })
            .collect::<Vec<_>>();

        let details = VotesDetails::new(val_set, &votes);

        Self { votes, details }
    }
}

#[derive(Serialize)]
struct RpcRoundValue {
    value: Value,
    round: i64,
}

impl From<&StateDump<ArcContext>> for RpcConsensusStateDump {
    fn from(s: &StateDump<ArcContext>) -> Self {
        RpcConsensusStateDump {
            address: s.address,
            proposer: s.proposer,
            validator_set: RpcValidatorSet::from(&s.validator_set),
            consensus: RpcConsensus::from(&s.consensus),
            vote_keeper: RpcVoteKeeperState::new(&s.vote_keeper, &s.validator_set),
        }
    }
}

impl From<&ValidatorSet> for RpcValidatorSet {
    fn from(vs: &ValidatorSet) -> Self {
        RpcValidatorSet {
            total_voting_power: vs.total_voting_power(),
            count: vs.len(),
            validators: vs.iter().map(RpcValidator::from).collect(),
        }
    }
}

impl From<&Validator> for RpcValidator {
    fn from(v: &Validator) -> Self {
        RpcValidator {
            address: v.address,
            voting_power: v.voting_power,
            public_key: v.public_key,
        }
    }
}

impl From<&MState<ArcContext>> for RpcConsensus {
    fn from(c: &MState<ArcContext>) -> Self {
        RpcConsensus {
            height: c.height.as_u64(),
            round: c.round.as_i64(),
            step: format!("{:?}", c.step),
            locked: c.locked.as_ref().map(RpcRoundValue::from),
            valid: c.valid.as_ref().map(RpcRoundValue::from),
            decision: c.decision.as_ref().map(RpcRoundValue::from),
        }
    }
}

impl From<&RoundValue<Value>> for RpcRoundValue {
    fn from(rv: &RoundValue<Value>) -> Self {
        RpcRoundValue {
            value: rv.value.clone(),
            round: rv.round.as_i64(),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum RequestError {
    Full,
    Closed,
    Recv,
}

impl From<AppRequestError> for RequestError {
    fn from(e: AppRequestError) -> Self {
        match e {
            AppRequestError::Full => RequestError::Full,
            AppRequestError::Closed => RequestError::Closed,
            AppRequestError::Recv => RequestError::Recv,
        }
    }
}

impl From<ConsensusRequestError> for RequestError {
    fn from(e: ConsensusRequestError) -> Self {
        match e {
            ConsensusRequestError::Full => RequestError::Full,
            ConsensusRequestError::Closed => RequestError::Closed,
            ConsensusRequestError::Recv => RequestError::Recv,
        }
    }
}

pub(crate) fn request_error_to_response(
    err: impl Into<RequestError>,
) -> (StatusCode, Json<serde_json::Value>) {
    match err.into() {
        RequestError::Full => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "Too many requests. Please retry later."})),
        ),
        RequestError::Closed => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Request channel is closed"})),
        ),
        RequestError::Recv => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to receive response"})),
        ),
    }
}

pub(crate) fn persistent_peer_error_to_response(
    err: PersistentPeerError,
) -> (StatusCode, Json<serde_json::Value>) {
    match err {
        PersistentPeerError::AlreadyExists => (
            StatusCode::CONFLICT,
            Json(json!({"error": "Persistent peer already exists"})),
        ),
        PersistentPeerError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Persistent peer not found"})),
        ),
        PersistentPeerError::NetworkStopped => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Network not started"})),
        ),
        PersistentPeerError::InternalError(msg) => {
            error!(%msg, "Persistent peer operation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Internal error"})),
            )
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcMisbehaviorEvidence {
    height: u64,
    validators: Vec<RpcValidatorEvidence>,
}

#[derive(Serialize)]
struct RpcValidatorEvidence {
    address: Address,
    double_votes: Vec<RpcDoubleVote>,
    double_proposals: Vec<RpcDoubleProposal>,
}

#[serde_as]
#[derive(Serialize)]
struct RpcDoubleVote {
    height: u64,
    round: i64,
    vote_type: String,
    #[serde_as(as = "Base64")]
    first_signature: Vec<u8>,
    #[serde_as(as = "Base64")]
    second_signature: Vec<u8>,
    first_value_id: Option<ValueId>,
    second_value_id: Option<ValueId>,
}

#[serde_as]
#[derive(Serialize)]
struct RpcDoubleProposal {
    height: u64,
    round: i64,
    #[serde_as(as = "Base64")]
    first_signature: Vec<u8>,
    #[serde_as(as = "Base64")]
    second_signature: Vec<u8>,
    first_value_id: ValueId,
    second_value_id: ValueId,
}

impl From<StoredMisbehaviorEvidence> for RpcMisbehaviorEvidence {
    fn from(e: StoredMisbehaviorEvidence) -> Self {
        RpcMisbehaviorEvidence {
            height: e.height.as_u64(),
            validators: e.validators.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ValidatorEvidence> for RpcValidatorEvidence {
    fn from(e: ValidatorEvidence) -> Self {
        RpcValidatorEvidence {
            address: e.address,
            double_votes: e.double_votes.into_iter().map(Into::into).collect(),
            double_proposals: e.double_proposals.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DoubleVote> for RpcDoubleVote {
    fn from(v: DoubleVote) -> Self {
        RpcDoubleVote {
            height: v.first.message.height.as_u64(),
            round: v.first.message.round.as_i64(),
            vote_type: format!("{:?}", v.first.message.typ),
            first_signature: v.first.signature.to_bytes().to_vec(),
            second_signature: v.second.signature.to_bytes().to_vec(),
            first_value_id: match v.first.message.value {
                NilOrVal::Nil => None,
                NilOrVal::Val(id) => Some(id),
            },
            second_value_id: match v.second.message.value {
                NilOrVal::Nil => None,
                NilOrVal::Val(id) => Some(id),
            },
        }
    }
}

impl From<DoubleProposal> for RpcDoubleProposal {
    fn from(p: DoubleProposal) -> Self {
        RpcDoubleProposal {
            height: p.first.message.height.as_u64(),
            round: p.first.message.round.as_i64(),
            first_signature: p.first.signature.to_bytes().to_vec(),
            second_signature: p.second.signature.to_bytes().to_vec(),
            first_value_id: p.first.message.value.id(),
            second_value_id: p.second.message.value.id(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RpcInvalidPayloads {
    height: u64,
    payloads: Vec<RpcInvalidPayload>,
}

#[derive(Serialize)]
struct RpcInvalidPayload {
    height: u64,
    round: i64,
    proposer_address: Address,
    payload: Option<ExecutionPayloadV3>,
    reason: String,
}

impl From<StoredInvalidPayloads> for RpcInvalidPayloads {
    fn from(s: StoredInvalidPayloads) -> Self {
        RpcInvalidPayloads {
            height: s.height.as_u64(),
            payloads: s.payloads.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<InvalidPayload> for RpcInvalidPayload {
    fn from(p: InvalidPayload) -> Self {
        RpcInvalidPayload {
            height: p.height.as_u64(),
            round: p.round.as_i64(),
            proposer_address: p.proposer_address,
            payload: p.payload,
            reason: p.reason,
        }
    }
}

#[cfg(test)]
mod tests_network {
    use super::*;

    #[test]
    fn test_network_validator_set_empty() {
        let vs: Vec<(String, u64)> = vec![];
        let rpc = build_network_validator_set(&vs);
        assert_eq!(rpc.total_voting_power, 0);
        assert_eq!(rpc.count, 0);
        assert!(rpc.validators.is_empty());
    }

    #[test]
    fn test_network_validator_set_basic() {
        let vs = vec![
            ("addr1".to_string(), 10),
            ("addr2".to_string(), 20),
            ("addr3".to_string(), 30),
        ];

        let rpc = build_network_validator_set(&vs);

        assert_eq!(rpc.total_voting_power, 60);
        assert_eq!(rpc.count, 3);
        assert_eq!(rpc.validators.len(), 3);

        assert_eq!(rpc.validators[0].address, "addr1");
        assert_eq!(rpc.validators[0].voting_power, 10);
        assert_eq!(rpc.validators[1].address, "addr2");
        assert_eq!(rpc.validators[1].voting_power, 20);
        assert_eq!(rpc.validators[2].address, "addr3");
        assert_eq!(rpc.validators[2].voting_power, 30);
    }
}

#[cfg(test)]
mod tests_errors {
    use super::*;

    #[test]
    fn test_request_error_full_maps_to_429() {
        let (status, body) = request_error_to_response(RequestError::Full);
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            body.0,
            json!({"error": "Too many requests. Please retry later."})
        );
    }

    #[test]
    fn test_request_error_closed_maps_to_500() {
        let (status, body) = request_error_to_response(RequestError::Closed);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0, json!({"error": "Request channel is closed"}));
    }

    #[test]
    fn test_request_error_recv_maps_to_500() {
        let (status, body) = request_error_to_response(RequestError::Recv);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0, json!({"error": "Failed to receive response"}));
    }
}

#[cfg(test)]
mod tests_misbehavior_evidence {
    use super::*;
    use alloy_primitives::Address as AlloyAddress;
    use arc_consensus_types::signing::Signature;
    use arc_consensus_types::{Address, Height, Round, Value, Vote};
    use malachitebft_core_types::{NilOrVal, SignedMessage, SignedProposal};

    fn create_test_vote(
        height: Height,
        round: Round,
        value: NilOrVal<ValueId>,
        address: Address,
        sig_byte: u8,
    ) -> SignedMessage<ArcContext, Vote> {
        let vote = Vote::new_precommit(height, round, value, address);
        let signature = Signature::from_bytes([sig_byte; 64]);
        SignedMessage::new(vote, signature)
    }

    fn create_test_proposal(
        height: Height,
        round: Round,
        value: Value,
        address: Address,
        sig_byte: u8,
    ) -> SignedProposal<ArcContext> {
        let proposal =
            arc_consensus_types::Proposal::new(height, round, value, Round::Nil, address);
        let signature = Signature::from_bytes([sig_byte; 64]);
        SignedProposal::new(proposal, signature)
    }

    #[test]
    fn test_empty_evidence_conversion() {
        let evidence = StoredMisbehaviorEvidence {
            height: Height::new(100),
            validators: vec![],
        };

        let rpc: RpcMisbehaviorEvidence = evidence.into();

        assert_eq!(rpc.height, 100);
        assert!(rpc.validators.is_empty());
    }

    #[test]
    fn test_evidence_with_double_votes() {
        let height = Height::new(42);
        let round = Round::new(1);
        let address = Address::from(AlloyAddress::from([0xaa; 20]));

        let value_id_1 = ValueId::new(BlockHash::repeat_byte(0x11));
        let value_id_2 = ValueId::new(BlockHash::repeat_byte(0x22));

        let vote1 = create_test_vote(height, round, NilOrVal::Val(value_id_1), address, 0x01);
        let vote2 = create_test_vote(height, round, NilOrVal::Val(value_id_2), address, 0x02);

        let double_vote = DoubleVote {
            first: vote1,
            second: vote2,
        };

        let validator_evidence = ValidatorEvidence {
            address,
            double_votes: vec![double_vote],
            double_proposals: vec![],
        };

        let evidence = StoredMisbehaviorEvidence {
            height,
            validators: vec![validator_evidence],
        };

        let rpc: RpcMisbehaviorEvidence = evidence.into();

        assert_eq!(rpc.height, 42);
        assert_eq!(rpc.validators.len(), 1);
        assert_eq!(rpc.validators[0].address, address);
        assert_eq!(rpc.validators[0].double_votes.len(), 1);
        assert!(rpc.validators[0].double_proposals.is_empty());

        let dv = &rpc.validators[0].double_votes[0];
        assert_eq!(dv.round, 1);
        assert_eq!(dv.vote_type, "Precommit");
        assert_eq!(dv.first_signature, vec![0x01; 64]);
        assert_eq!(dv.second_signature, vec![0x02; 64]);
        assert_eq!(dv.first_value_id, Some(value_id_1));
        assert_eq!(dv.second_value_id, Some(value_id_2));
    }

    #[test]
    fn test_evidence_with_nil_votes() {
        let height = Height::new(50);
        let round = Round::new(2);
        let address = Address::from(AlloyAddress::from([0xbb; 20]));

        let value_id = ValueId::new(BlockHash::repeat_byte(0x33));

        // One vote for a value, one nil vote
        let vote1 = create_test_vote(height, round, NilOrVal::Val(value_id), address, 0x03);
        let vote2 = create_test_vote(height, round, NilOrVal::Nil, address, 0x04);

        let double_vote = DoubleVote {
            first: vote1,
            second: vote2,
        };

        let validator_evidence = ValidatorEvidence {
            address,
            double_votes: vec![double_vote],
            double_proposals: vec![],
        };

        let evidence = StoredMisbehaviorEvidence {
            height,
            validators: vec![validator_evidence],
        };

        let rpc: RpcMisbehaviorEvidence = evidence.into();

        let dv = &rpc.validators[0].double_votes[0];
        assert_eq!(dv.first_value_id, Some(value_id));
        assert_eq!(dv.second_value_id, None);
    }

    #[test]
    fn test_evidence_with_double_proposals() {
        let height = Height::new(99);
        let round = Round::new(0);
        let address = Address::from(AlloyAddress::from([0xcc; 20]));

        let value1 = Value::new(BlockHash::repeat_byte(0x44));
        let value2 = Value::new(BlockHash::repeat_byte(0x55));

        let proposal1 = create_test_proposal(height, round, value1.clone(), address, 0x05);
        let proposal2 = create_test_proposal(height, round, value2.clone(), address, 0x06);

        let double_proposal = DoubleProposal {
            first: proposal1,
            second: proposal2,
        };

        let validator_evidence = ValidatorEvidence {
            address,
            double_votes: vec![],
            double_proposals: vec![double_proposal],
        };

        let evidence = StoredMisbehaviorEvidence {
            height,
            validators: vec![validator_evidence],
        };

        let rpc: RpcMisbehaviorEvidence = evidence.into();

        assert_eq!(rpc.height, 99);
        assert_eq!(rpc.validators.len(), 1);
        assert!(rpc.validators[0].double_votes.is_empty());
        assert_eq!(rpc.validators[0].double_proposals.len(), 1);

        let dp = &rpc.validators[0].double_proposals[0];
        assert_eq!(dp.round, 0);
        assert_eq!(dp.first_signature, vec![0x05; 64]);
        assert_eq!(dp.second_signature, vec![0x06; 64]);
        assert_eq!(dp.first_value_id, value1.id());
        assert_eq!(dp.second_value_id, value2.id());
    }

    #[test]
    fn test_evidence_with_multiple_validators() {
        let height = Height::new(200);
        let round = Round::new(3);

        let addr1 = Address::from(AlloyAddress::from([0xdd; 20]));
        let addr2 = Address::from(AlloyAddress::from([0xee; 20]));

        let value_id_1 = ValueId::new(BlockHash::repeat_byte(0x66));
        let value_id_2 = ValueId::new(BlockHash::repeat_byte(0x77));

        // First validator with vote equivocation
        let vote1 = create_test_vote(height, round, NilOrVal::Val(value_id_1), addr1, 0x07);
        let vote2 = create_test_vote(height, round, NilOrVal::Val(value_id_2), addr1, 0x08);

        let double_vote = DoubleVote {
            first: vote1,
            second: vote2,
        };

        let validator1 = ValidatorEvidence {
            address: addr1,
            double_votes: vec![double_vote],
            double_proposals: vec![],
        };

        // Second validator with proposal equivocation
        let value1 = Value::new(BlockHash::repeat_byte(0x88));
        let value2 = Value::new(BlockHash::repeat_byte(0x99));

        let proposal1 = create_test_proposal(height, round, value1, addr2, 0x09);
        let proposal2 = create_test_proposal(height, round, value2, addr2, 0x0a);

        let double_proposal = DoubleProposal {
            first: proposal1,
            second: proposal2,
        };

        let validator2 = ValidatorEvidence {
            address: addr2,
            double_votes: vec![],
            double_proposals: vec![double_proposal],
        };

        let evidence = StoredMisbehaviorEvidence {
            height,
            validators: vec![validator1, validator2],
        };

        let rpc: RpcMisbehaviorEvidence = evidence.into();

        assert_eq!(rpc.height, 200);
        assert_eq!(rpc.validators.len(), 2);

        // First validator
        assert_eq!(rpc.validators[0].address, addr1);
        assert_eq!(rpc.validators[0].double_votes.len(), 1);
        assert!(rpc.validators[0].double_proposals.is_empty());

        // Second validator
        assert_eq!(rpc.validators[1].address, addr2);
        assert!(rpc.validators[1].double_votes.is_empty());
        assert_eq!(rpc.validators[1].double_proposals.len(), 1);
    }
}
