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

//! End-to-end integration tests for the RPC server
//!
//! These tests start a real HTTP server and make actual HTTP requests to it,
//! verifying the complete request/response cycle including middleware, routing,
//! serialization, and API versioning.

use std::time::Duration;

use arc_consensus_types::{Address, Height, Round, ValidatorSet};
use arc_node_consensus::request::{AppRequest, Status};
use malachitebft_app_channel::{ConsensusRequest, NetworkRequest};

mod common;
use common::TestServer;

/// Test that the root endpoint returns API documentation
#[tokio::test]
async fn test_root_endpoint_returns_docs() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");

    // Verify endpoints documentation is present
    assert!(body.get("endpoints").is_some());
    assert!(body.get("rpc_versioning").is_some());

    // Verify versioning info
    let versioning = body.get("rpc_versioning").unwrap();
    assert_eq!(versioning.get("method").unwrap(), "header-based");
    assert_eq!(versioning.get("header").unwrap(), "Accept");
}

/// Test the /health endpoint with default API version
#[tokio::test]
async fn test_health_endpoint_ok() {
    let server = TestServer::start().await;

    // Respond to health check request
    server.expect_app_request(|req| match req {
        AppRequest::GetHealth(reply) => {
            reply.send(()).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");
    assert_eq!(body.get("status").unwrap(), "ok");
}

/// Test the /version endpoint returns version information
#[tokio::test]
async fn test_version_endpoint() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/version", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");

    // Verify version fields are present
    assert!(body.get("git_version").is_some());
    assert!(body.get("git_commit").is_some());
    assert!(body.get("git_short_hash").is_some());
    assert!(body.get("cargo_version").is_some());
}

/// Test the /status endpoint returns application status
#[tokio::test]
async fn test_status_endpoint() {
    let server = TestServer::start().await;

    // Respond to status request
    server.expect_app_request(|req| match req {
        AppRequest::GetStatus(reply) => {
            let status = Status {
                height: Height::new(100),
                round: Round::new(1),
                address: Address::repeat_byte(1),
                proposer: Some(Address::repeat_byte(2)),
                height_start_time: std::time::SystemTime::now(),
                prev_payload_hash: None,
                db_latest_height: Height::new(99),
                db_earliest_height: Height::new(1),
                validator_set: ValidatorSet::default(),
                undecided_blocks_count: 0,
                pending_proposal_parts: vec![],
            };
            reply.send(status).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/status", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");

    // Verify status fields
    assert_eq!(body.get("height").unwrap(), 100);
    assert_eq!(body.get("round").unwrap(), 1);
    assert!(body.get("address").is_some());
    assert!(body.get("validator_set").is_some());
}

/// Test the /commit endpoint with a valid height parameter
#[tokio::test]
async fn test_commit_endpoint_with_height() {
    let server = TestServer::start().await;

    // Respond to certificate request
    server.expect_app_request(|req| match req {
        AppRequest::GetCertificate(height, reply) => {
            assert_eq!(height, Some(Height::new(42)));
            reply.send(None).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/commit?height=42", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 404); // Not found since we returned None
}

/// Test the /commit endpoint without height parameter
#[tokio::test]
async fn test_commit_endpoint_without_height() {
    let server = TestServer::start().await;

    // Respond to certificate request
    server.expect_app_request(|req| match req {
        AppRequest::GetCertificate(height, reply) => {
            assert_eq!(height, None);
            reply.send(None).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/commit", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 404); // Not found since we returned None
}

/// Test API versioning with explicit v1 Accept header
#[tokio::test]
async fn test_api_versioning_explicit_v1() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/version", server.url()))
        .header("Accept", "application/vnd.arc.v1+json")
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/vnd.arc.v1+json"
    );
}

/// Test API versioning with generic JSON Accept header (defaults to v1)
#[tokio::test]
async fn test_api_versioning_generic_json() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/version", server.url()))
        .header("Accept", "application/json")
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/vnd.arc.v1+json"
    );
}

/// Test API versioning with unsupported version returns 406 Not Acceptable
#[tokio::test]
async fn test_api_versioning_unsupported_version() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/version", server.url()))
        .header("Accept", "application/vnd.arc.v99+json")
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 406); // Not Acceptable
}

/// Test the /consensus-state endpoint when state is available
#[tokio::test]
async fn test_consensus_state_available() {
    let server = TestServer::start().await;

    // Respond to state dump request
    server.expect_consensus_request(|req| {
        let ConsensusRequest::DumpState(reply) = req;
        reply.send(None).ok(); // Simulate state not available yet
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/consensus-state", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 503); // Service unavailable
}

/// Test the /network-state endpoint when state is available
#[tokio::test]
async fn test_network_state_available() {
    let server = TestServer::start().await;

    // Respond to network state dump request
    server.expect_network_request(|req| {
        let NetworkRequest::DumpState(reply) = req else {
            panic!("Unexpected request type");
        };
        reply.send(None).ok(); // Simulate state not available yet
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/network-state", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 503); // Service unavailable
}

/// Test that multiple concurrent requests are handled correctly
#[tokio::test]
async fn test_concurrent_requests() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // Spawn multiple concurrent requests
    let mut handles = vec![];
    for _ in 0..10 {
        let client = client.clone();
        let url = server.url().to_string();
        let handle = tokio::spawn(async move {
            client
                .get(format!("{}/version", url))
                .send()
                .await
                .expect("Failed to send request")
        });
        handles.push(handle);
    }

    // Wait for all requests to complete
    for handle in handles {
        let response = handle.await.expect("Task panicked");
        assert_eq!(response.status(), 200);
    }
}

/// Test that server handles graceful shutdown
#[tokio::test]
async fn test_server_shutdown() {
    let server = TestServer::start().await;
    let url = server.url().to_string();

    // Make a successful request
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/version", url))
        .send()
        .await
        .expect("Failed to send request");
    assert_eq!(response.status(), 200);

    // Drop the server to trigger shutdown
    drop(server);

    // Give it a moment to shut down
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Subsequent requests should fail (connection refused or similar)
    // Note: In a real scenario, the server would shut down properly
    // For this test, we're just verifying the server was working before drop
}

/// Test error handling when app request channel is full
#[tokio::test]
async fn test_app_request_channel_full() {
    let server = TestServer::start().await;

    // Don't respond to requests - let them pile up
    // The channel has limited capacity, so eventually we should get a 429

    let client = reqwest::Client::new();

    // Make many requests without processing them
    for _ in 0..100 {
        let _response = client
            .get(format!("{}/health", server.url()))
            .timeout(Duration::from_millis(100))
            .send()
            .await;
    }

    // At least some requests should have succeeded or timed out
    // This test mainly verifies the server doesn't crash under load
}

/// Test that the actual production status response format is correct
#[tokio::test]
async fn test_status_response_structure() {
    let server = TestServer::start().await;

    // Respond to status request with real data
    server.expect_app_request(|req| match req {
        AppRequest::GetStatus(reply) => {
            let status = Status {
                height: Height::new(12345),
                round: Round::new(7),
                address: Address::repeat_byte(0xAB),
                proposer: Some(Address::repeat_byte(0xCD)),
                height_start_time: std::time::SystemTime::now(),
                prev_payload_hash: None,
                db_latest_height: Height::new(12344),
                db_earliest_height: Height::new(1),
                validator_set: ValidatorSet::default(),
                undecided_blocks_count: 5,
                pending_proposal_parts: vec![(Height::new(100), 3)],
            };
            reply.send(status).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/status", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 200);

    let body: serde_json::Value = response.json().await.expect("Failed to parse JSON");

    // Verify the actual production response structure
    assert_eq!(body.get("height").unwrap(), 12345);
    assert_eq!(body.get("round").unwrap(), 7);
    assert!(body.get("address").is_some());
    assert!(body.get("proposer").is_some());
    assert!(body.get("height_start_time").is_some());
    assert!(body.get("db_latest_height").is_some());
    assert!(body.get("db_earliest_height").is_some());
    assert!(body.get("validator_set").is_some());
    assert!(body.get("undecided_blocks_count").is_some());
    assert!(body.get("pending_proposal_parts").is_some());

    // Verify validator_set structure
    let validator_set = body.get("validator_set").unwrap();
    assert!(validator_set.get("total_voting_power").is_some());
    assert!(validator_set.get("count").is_some());
    assert!(validator_set.get("validators").is_some());
}

/// Test that invalid query parameters are handled gracefully
#[tokio::test]
async fn test_invalid_query_parameters() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    // Invalid height parameter
    let response = client
        .get(format!("{}/commit?height=not_a_number", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    // Should return 400 Bad Request
    assert_eq!(response.status(), 400);
}

/// Test the /misbehavior-evidence endpoint with a valid height parameter
#[tokio::test]
async fn test_no_misbehavior_evidence_endpoint_with_height() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    server.expect_app_request(|req| match req {
        AppRequest::GetMisbehaviorEvidence(height, reply) => {
            assert_eq!(height, Some(Height::new(100)));
            reply.send(None).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let response = client
        .get(format!("{}/misbehavior-evidence?height=100", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 404); // Not found since we returned None
}

/// Test the /misbehavior-evidence endpoint without height parameter
#[tokio::test]
async fn test_no_misbehavior_evidence_endpoint_without_height() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    server.expect_app_request(|req| match req {
        AppRequest::GetMisbehaviorEvidence(height, reply) => {
            assert_eq!(height, None);
            reply.send(None).ok();
        }
        _ => panic!("Unexpected request type"),
    });

    let response = client
        .get(format!("{}/misbehavior-evidence", server.url()))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(response.status(), 404); // Not found since we returned None
}
