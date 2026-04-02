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

//! Hardcoded configuration values that must be consistent across all nodes.

use std::time::Duration;

use arc_consensus_types::{ConsensusConfig, RemoteSigningConfig, RetryConfig, ValueSyncConfig};
use arc_node_consensus_cli::config::ScoringStrategy;
use malachitebft_app_channel::app::config::{
    BootstrapProtocol, DiscoveryConfig, GossipSubConfig, P2pConfig, PubSubProtocol, Selector,
    ValuePayload,
};
use malachitebft_app_channel::app::net::Multiaddr;

/// Hardcoded consensus parameters that must be consistent across all nodes.
pub mod consensus {
    use super::*;

    /// Message types required by consensus to deliver the value being proposed.
    ///
    /// Default: `ValuePayload::PartsOnly`
    pub const VALUE_PAYLOAD: ValuePayload = ValuePayload::ProposalAndParts;

    /// Capacity of the consensus message queue, in heights.
    ///
    /// Default: `10`
    pub const QUEUE_CAPACITY: usize = 16;
}

/// Hardcoded discovery parameters.
pub mod discovery {
    use super::*;

    /// Bootstrap protocol for discovery.
    /// "full" bootstrap is faster for small (< 1000) networks.
    /// Kademlia bootstrap is slower but can scale to large (> 1000) nodes network.
    ///
    /// Default: `BootstrapProtocol::Kademlia`
    pub const BOOTSTRAP_PROTOCOL: BootstrapProtocol = BootstrapProtocol::Full;

    /// Peer selector for discovery.
    /// For Arc, we use random selector with full bootstrap.
    ///
    /// Default: `Selector::Kademlia`
    pub const SELECTOR: Selector = Selector::Random;

    /// Default: `Duration::from_secs(60)`
    pub const EPHEMERAL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
}

/// Hardcoded remote signing parameters.
pub mod remote_signing {
    use super::*;

    /// Timeout for remote signing requests.
    pub const TIMEOUT: Duration = Duration::from_secs(30);
}

/// Hardcoded sync parameters.
pub mod value_sync {
    use bytesize::ByteSize;

    use super::*;

    /// Interval at which to update other peers of our status
    /// Default: `Duration::from_secs(10)`
    ///
    /// Note: Setting this to 0 means "update status on every block".
    pub const STATUS_UPDATE_INTERVAL: Duration = Duration::from_secs(0);

    /// Timeout duration for sync requests
    /// Default: `Duration::from_secs(10)`
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

    /// Maximum size of a request
    /// Default: `1 MiB`
    pub const MAX_REQUEST_SIZE: ByteSize = ByteSize::mib(1);

    /// Maximum size of a response
    /// Default: `10 MiB`
    pub const MAX_RESPONSE_SIZE: ByteSize = ByteSize::mib(10);

    /// Maximum number of parallel requests to send
    /// Default: `5`
    pub const PARALLEL_REQUESTS: usize = 5;

    /// Threshold for considering a peer inactive
    /// Default: `Duration::from_secs(60)` (1 minute)
    pub const INACTIVE_THRESHOLD: Duration = Duration::from_secs(60);

    /// Maximum number of decided values to request in a single batch
    /// Default: `5`
    pub const BATCH_SIZE: usize = 10;
}

/// Gossipsub network load profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GossipLoad {
    /// Fewer mesh peers, lower bandwidth (mesh_n=3)
    Low,
    /// Balanced for typical deployments (mesh_n=6)
    #[default]
    Average,
    /// More mesh peers, higher bandwidth (mesh_n=10)
    High,
}

impl GossipLoad {
    /// Parse from an optional CLI string. Unrecognized or missing values default to `Average`.
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("low") => Self::Low,
            Some("high") => Self::High,
            _ => Self::Average,
        }
    }

    /// Canonical mesh parameters for this load profile.
    pub fn mesh_params(&self) -> GossipMeshParams {
        match self {
            Self::Low => GossipMeshParams {
                mesh_n: 3,
                mesh_n_high: 4,
                mesh_n_low: 1,
                mesh_outbound_min: 1,
            },
            Self::Average => GossipMeshParams {
                mesh_n: 6,
                mesh_n_high: 12,
                mesh_n_low: 4,
                mesh_outbound_min: 2,
            },
            Self::High => GossipMeshParams {
                mesh_n: 10,
                mesh_n_high: 15,
                mesh_n_low: 5,
                mesh_outbound_min: 3,
            },
        }
    }
}

/// Named gossipsub mesh sizing parameters.
#[derive(Clone, Debug)]
pub struct GossipMeshParams {
    pub mesh_n: usize,
    pub mesh_n_high: usize,
    pub mesh_n_low: usize,
    pub mesh_outbound_min: usize,
}

/// CLI provided gossipsub overrides.
#[derive(Clone, Debug, Default)]
pub struct GossipSubOverrides {
    pub explicit_peering: bool,
    pub mesh_prioritization: bool,
    pub load: GossipLoad,
}

/// Build a consensus configuration with hardcoded parameters.
#[allow(clippy::too_many_arguments)]
pub fn build_consensus_config(
    listen_addr: Multiaddr,
    persistent_peers: Vec<Multiaddr>,
    persistent_peers_only: bool,
    discovery_enabled: bool,
    num_outbound_peers: usize,
    num_inbound_peers: usize,
    consensus_enabled: bool,
    gossipsub_overrides: GossipSubOverrides,
) -> ConsensusConfig {
    ConsensusConfig {
        enabled: consensus_enabled,
        value_payload: consensus::VALUE_PAYLOAD,
        queue_capacity: consensus::QUEUE_CAPACITY,
        p2p: P2pConfig {
            listen_addr,
            persistent_peers,
            persistent_peers_only,
            protocol: PubSubProtocol::GossipSub(generate_p2p_gossip_config(gossipsub_overrides)),
            discovery: DiscoveryConfig {
                enabled: discovery_enabled,
                bootstrap_protocol: discovery::BOOTSTRAP_PROTOCOL,
                selector: discovery::SELECTOR,
                num_outbound_peers,
                num_inbound_peers,
                ephemeral_connection_timeout: discovery::EPHEMERAL_CONNECTION_TIMEOUT,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

fn generate_p2p_gossip_config(overrides: GossipSubOverrides) -> GossipSubConfig {
    let p = overrides.load.mesh_params();

    GossipSubConfig::new(
        p.mesh_n,
        p.mesh_n_high,
        p.mesh_n_low,
        p.mesh_outbound_min,
        overrides.mesh_prioritization,
        overrides.explicit_peering,
        true, // flood_publish
    )
}

/// Build a value sync configuration with hardcoded parameters.
pub fn build_value_sync_config(enabled: bool) -> ValueSyncConfig {
    ValueSyncConfig {
        enabled,
        status_update_interval: value_sync::STATUS_UPDATE_INTERVAL,
        request_timeout: value_sync::REQUEST_TIMEOUT,
        max_request_size: value_sync::MAX_REQUEST_SIZE,
        max_response_size: value_sync::MAX_RESPONSE_SIZE,
        parallel_requests: value_sync::PARALLEL_REQUESTS,
        scoring_strategy: ScoringStrategy::Ema,
        inactive_threshold: value_sync::INACTIVE_THRESHOLD,
        batch_size: value_sync::BATCH_SIZE,
    }
}

/// Build a remote signing configuration with hardcoded parameters.
pub fn build_remote_signing_config(
    endpoint: String,
    tls_cert_path: Option<String>,
) -> RemoteSigningConfig {
    RemoteSigningConfig {
        endpoint,
        timeout: remote_signing::TIMEOUT,
        retry: RetryConfig::default(),
        enable_tls: tls_cert_path.is_some(), // auto-enable if cert path provided
        tls_cert_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malachitebft_app_channel::app::config::PubSubProtocol;

    #[test]
    fn generate_gossipsub_config_uses_defaults() {
        let config = GossipSubConfig::default();

        assert_eq!(config.mesh_n(), 6);
        assert_eq!(config.mesh_n_low(), 4);
        assert_eq!(config.mesh_n_high(), 12);
        assert_eq!(config.mesh_outbound_min(), 2);
    }

    fn default_overrides() -> GossipSubOverrides {
        GossipSubOverrides::default()
    }

    #[test]
    fn build_consensus_config_sets_hardcoded_values() {
        let listen_addr: Multiaddr = "/ip4/127.0.0.1/tcp/27000".parse().unwrap();
        let peer1: Multiaddr = "/ip4/127.0.0.1/tcp/27001".parse().unwrap();
        let peer2: Multiaddr = "/ip4/127.0.0.1/tcp/27002".parse().unwrap();
        let persistent_peers = vec![peer1.clone(), peer2.clone()];

        let config = build_consensus_config(
            listen_addr.clone(),
            persistent_peers.clone(),
            false, // persistent_peers_only
            false,
            20,
            20,
            true, // consensus enabled
            default_overrides(),
        );

        assert!(config.enabled);
        assert_eq!(config.value_payload, consensus::VALUE_PAYLOAD);
        assert_eq!(config.queue_capacity, consensus::QUEUE_CAPACITY);
        assert_eq!(config.p2p.listen_addr, listen_addr);
        assert_eq!(config.p2p.persistent_peers, persistent_peers);
    }

    #[test]
    fn build_consensus_config_with_discovery_enabled() {
        let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/27000".parse().unwrap();

        let config = build_consensus_config(
            listen_addr,
            vec![],
            false, // persistent_peers_only
            true,  // discovery enabled
            30,
            40,
            true, // consensus enabled
            default_overrides(),
        );

        assert!(config.p2p.discovery.enabled);
        assert_eq!(config.p2p.discovery.num_outbound_peers, 30);
        assert_eq!(config.p2p.discovery.num_inbound_peers, 40);
        assert_eq!(
            config.p2p.discovery.bootstrap_protocol,
            discovery::BOOTSTRAP_PROTOCOL
        );
        assert_eq!(config.p2p.discovery.selector, discovery::SELECTOR);
        assert_eq!(
            config.p2p.discovery.ephemeral_connection_timeout,
            discovery::EPHEMERAL_CONNECTION_TIMEOUT
        );
    }

    #[test]
    fn build_consensus_config_with_discovery_disabled() {
        let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/27000".parse().unwrap();

        let config = build_consensus_config(
            listen_addr,
            vec![],
            false, // persistent_peers_only
            false, // discovery disabled
            20,
            20,
            true, // consensus enabled
            default_overrides(),
        );

        assert!(!config.p2p.discovery.enabled);
    }

    #[test]
    fn build_consensus_config_supports_tcp_multiaddr() {
        let listen_addr: Multiaddr = "/ip4/172.19.0.5/tcp/27000".parse().unwrap();

        let config = build_consensus_config(
            listen_addr.clone(),
            vec![],
            false,
            false,
            20,
            20,
            true,
            default_overrides(),
        );

        assert_eq!(config.p2p.listen_addr, listen_addr);
    }

    #[test]
    fn build_consensus_config_supports_quic_multiaddr() {
        let listen_addr: Multiaddr = "/ip4/127.0.0.1/udp/27000/quic-v1".parse().unwrap();

        let config = build_consensus_config(
            listen_addr.clone(),
            vec![],
            false,
            false,
            20,
            20,
            true,
            default_overrides(),
        );

        assert_eq!(config.p2p.listen_addr, listen_addr);
    }

    #[test]
    fn build_consensus_config_uses_gossipsub_protocol() {
        let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/27000".parse().unwrap();

        let config = build_consensus_config(
            listen_addr,
            vec![],
            false,
            false,
            20,
            20,
            true,
            default_overrides(),
        );

        // Verify it's using GossipSub protocol
        matches!(config.p2p.protocol, PubSubProtocol::GossipSub(_));
    }

    #[test]
    fn build_consensus_config_disabled() {
        let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/27000".parse().unwrap();

        let config = build_consensus_config(
            listen_addr,
            vec![],
            false,
            false,
            20,
            20,
            false,
            default_overrides(),
        );

        assert!(!config.enabled);
    }

    #[test]
    fn build_consensus_config_with_gossipsub_overrides() {
        let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/27000".parse().unwrap();

        let overrides = GossipSubOverrides {
            explicit_peering: true,
            mesh_prioritization: true,
            load: GossipLoad::High,
        };

        let config =
            build_consensus_config(listen_addr, vec![], false, false, 20, 20, true, overrides);

        if let PubSubProtocol::GossipSub(gs) = config.p2p.protocol {
            assert!(gs.enable_explicit_peering());
            assert!(gs.enable_peer_scoring());
            assert_eq!(gs.mesh_n(), 10);
            assert_eq!(gs.mesh_n_high(), 15);
            assert_eq!(gs.mesh_n_low(), 5);
            assert_eq!(gs.mesh_outbound_min(), 3);
        } else {
            panic!("Expected GossipSub protocol");
        }
    }

    #[test]
    fn build_value_sync_config_enabled() {
        let config = build_value_sync_config(true);

        assert!(config.enabled);
    }

    #[test]
    fn build_value_sync_config_disabled() {
        let config = build_value_sync_config(false);

        assert!(!config.enabled);
    }

    #[test]
    fn build_remote_signing_config_without_tls() {
        let endpoint = "http://signer:10340".to_string();
        let config = build_remote_signing_config(endpoint.clone(), None);

        assert_eq!(config.endpoint, endpoint);
        assert_eq!(config.timeout, remote_signing::TIMEOUT);
        assert!(!config.enable_tls);
        assert_eq!(config.tls_cert_path, None);
    }

    #[test]
    fn build_remote_signing_config_with_tls_cert() {
        let endpoint = "https://signer:10340".to_string();
        let cert_path = "/path/to/cert.pem".to_string();
        let config = build_remote_signing_config(endpoint.clone(), Some(cert_path.clone()));

        assert_eq!(config.endpoint, endpoint);
        assert_eq!(config.timeout, remote_signing::TIMEOUT);
        assert!(config.enable_tls); // Auto-enabled when cert path provided
        assert_eq!(config.tls_cert_path, Some(cert_path));
    }

    #[test]
    fn build_remote_signing_config_uses_hardcoded_timeout() {
        let config = build_remote_signing_config("http://signer:10340".to_string(), None);

        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.timeout, remote_signing::TIMEOUT);
    }

    #[test]
    fn build_remote_signing_config_uses_default_retry() {
        let config = build_remote_signing_config("http://signer:10340".to_string(), None);

        assert_eq!(config.retry, RetryConfig::default());
    }

    #[test]
    fn constants_have_expected_values() {
        // Verify consensus constants
        assert_eq!(consensus::VALUE_PAYLOAD, ValuePayload::ProposalAndParts);
        assert_eq!(consensus::QUEUE_CAPACITY, 16);

        // Verify discovery constants
        assert_eq!(discovery::BOOTSTRAP_PROTOCOL, BootstrapProtocol::Full);
        assert_eq!(discovery::SELECTOR, Selector::Random);
        assert_eq!(
            discovery::EPHEMERAL_CONNECTION_TIMEOUT,
            Duration::from_secs(5)
        );

        // Verify remote signing constants
        assert_eq!(remote_signing::TIMEOUT, Duration::from_secs(30));
    }
}
