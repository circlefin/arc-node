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

//! Arc Malachite application.

/// Number of certificate heights retained by the pruning presets.
/// Approximately 33 hours at 0.5 s/block (237600 × 0.5 s ≈ 33 h).
const PRESETS_PRUNE_CERTIFICATES_DISTANCE: u64 = 237_600;

use bytesize::ByteSize;
use eyre::{eyre, Result};
use tracing::{info, trace};

use arc_consensus_types::{
    Config, ExecutionConfig, Height, MetricsConfig, PruningConfig, RpcConfig, RuntimeConfig,
    SigningConfig,
};
use arc_node_consensus::hardcoded_config;
use arc_node_consensus::node::{App, StartConfig};
use arc_node_consensus::store::migrations::MigrationCoordinator;
use arc_node_consensus_cli::{
    args::{Args, Commands},
    cmd::{
        db::DbCommands, db::MigrateCmd, download::DownloadCmd, init::InitCmd, key::KeyCmd,
        start::StartCmd,
    },
    config, logging, runtime,
};

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Profiling configuration for jemalloc.
#[cfg(feature = "pprof")]
#[allow(non_upper_case_globals)]
#[unsafe(export_name = "malloc_conf")]
pub static malloc_conf: &[u8] = b"prof:true,prof_active:true,lg_prof_sample:19\0";

/// Main entry point for the application
///
/// This function:
/// - Parses command-line arguments
/// - Builds configuration from CLI arguments
/// - Initializes logging system
/// - Sets up error handling
/// - Creates and runs the application node
fn main() -> Result<()> {
    oneline_eyre::install()?;

    // Load command-line arguments and possible configuration file.
    let args = Args::new();

    // Override logging configuration (if exists) with optional command-line parameters.
    let mut logging = config::LoggingConfig::default();
    if let Some(log_level) = args.log_level {
        logging.log_level = log_level;
    }
    if let Some(log_format) = args.log_format {
        logging.log_format = log_format;
    }

    // This is a drop guard responsible for flushing any remaining logs when the program terminates.
    // It must be assigned to a binding that is not _, as _ will result in the guard being dropped immediately.
    let _guard = logging::init(logging.log_level, logging.log_format);

    info!(
        version = arc_version::GIT_VERSION,
        commit = arc_version::GIT_COMMIT_HASH,
        "Arc Consensus CL starting"
    );

    trace!("Command-line parameters: {args:?}");

    // Parse the input command.
    match &args.command {
        Commands::Start(cmd) => start(&args, cmd, logging),
        Commands::Init(cmd) => init(&args, cmd, logging),
        Commands::Key(cmd) => key(&args, cmd),
        Commands::Db(db_cmd) => match db_cmd {
            DbCommands::Migrate(cmd) => db_migrate(&args, cmd),
            DbCommands::Compact => compact(&args),
        },
        Commands::Download(cmd) => download(&args, cmd),
    }
}

/// Build signing configuration from CLI arguments
fn build_signing_config(cmd: &StartCmd) -> Result<SigningConfig> {
    if let Some(endpoint) = &cmd.signing_remote {
        Ok(SigningConfig::Remote(
            hardcoded_config::build_remote_signing_config(
                endpoint.clone(),
                cmd.signing_tls_cert_path.clone(),
            ),
        ))
    } else {
        // Default to local signing if neither flag is specified
        Ok(SigningConfig::Local)
    }
}

/// Build configuration from CLI arguments
fn build_config_from_cli(cmd: &StartCmd, logging: config::LoggingConfig) -> Result<Config> {
    let p2p_listen_addr = cmd.p2p_listen_addr()?;
    let persistent_peers = cmd.persistent_peers();

    // Consensus is disabled when --no-consensus is set or follow mode is enabled
    let consensus_enabled = !cmd.no_consensus && !cmd.follow;

    let gossipsub_overrides = hardcoded_config::GossipSubOverrides {
        explicit_peering: cmd.gossipsub_explicit_peering,
        mesh_prioritization: cmd.gossipsub_mesh_prioritization,
        load: hardcoded_config::GossipLoad::from_str_opt(cmd.gossipsub_load.as_deref()),
    };

    let consensus = hardcoded_config::build_consensus_config(
        p2p_listen_addr,
        persistent_peers.clone(),
        cmd.p2p_persistent_peers_only,
        cmd.discovery,
        cmd.discovery_num_outbound_peers,
        cmd.discovery_num_inbound_peers,
        consensus_enabled,
        gossipsub_overrides,
    );

    let value_sync = hardcoded_config::build_value_sync_config(cmd.value_sync);

    let metrics = MetricsConfig {
        enabled: cmd.metrics.is_some(),
        listen_addr: cmd
            .metrics
            .unwrap_or_else(|| "0.0.0.0:29000".parse().expect("valid socket address")),
    };

    let runtime = match cmd.runtime_flavor.as_str() {
        "single-threaded" => RuntimeConfig::single_threaded(),
        "multi-threaded" => RuntimeConfig::multi_threaded(cmd.worker_threads.unwrap_or(0)),
        _ => {
            return Err(eyre!(
                "Invalid runtime flavor: {}. Must be 'single-threaded' or 'multi-threaded'",
                cmd.runtime_flavor
            ));
        }
    };

    let rpc = RpcConfig {
        enabled: cmd.rpc_addr.is_some(),
        listen_addr: cmd
            .rpc_addr
            .unwrap_or_else(|| "0.0.0.0:31000".parse().expect("valid socket address")),
    };

    let certificates_distance = if cmd.full || cmd.minimal {
        PRESETS_PRUNE_CERTIFICATES_DISTANCE
    } else {
        cmd.prune_certificates_distance
    };
    let prune = PruningConfig {
        certificates_distance,
        certificates_before: Height::new(cmd.prune_certificates_before),
    };

    let execution = ExecutionConfig {
        persistence_backpressure: cmd.execution_persistence_backpressure,
        persistence_backpressure_threshold: cmd.execution_persistence_backpressure_threshold,
    };

    Ok(Config {
        moniker: cmd.get_moniker(),
        logging,
        consensus,
        value_sync,
        metrics,
        runtime,
        prune,
        rpc,
        execution,
        signing: build_signing_config(cmd)?,
    })
}

fn start(args: &Args, cmd: &StartCmd, logging: config::LoggingConfig) -> Result<()> {
    // Validate command options before proceeding
    cmd.validate()
        .map_err(|error| eyre!("Invalid command options: {error}"))?;

    // Build configuration from CLI arguments
    let config = build_config_from_cli(cmd, logging)?;

    let rt = runtime::build_runtime(config.runtime)?;

    info!(
        moniker = %config.moniker,
        p2p_addr = %config.consensus.p2p.listen_addr,
        "Built configuration from CLI arguments",
    );

    trace!(?config, "Configuration");

    let private_key_file = {
        let default = args.get_default_priv_validator_key_file_path()?;
        cmd.private_key_file(default)?
    };

    let start_config = StartConfig {
        persistent_peers: cmd.persistent_peers(),
        persistent_peers_only: cmd.p2p_persistent_peers_only,
        gossipsub_overrides: hardcoded_config::GossipSubOverrides {
            explicit_peering: cmd.gossipsub_explicit_peering,
            mesh_prioritization: cmd.gossipsub_mesh_prioritization,
            load: hardcoded_config::GossipLoad::from_str_opt(cmd.gossipsub_load.as_deref()),
        },
        eth_socket: cmd.eth_socket.clone(),
        execution_socket: cmd.execution_socket.clone(),
        eth_rpc_endpoint: cmd.eth_rpc_endpoint.clone(),
        execution_endpoint: cmd.execution_endpoint.clone(),
        execution_ws_endpoint: cmd.execution_ws_endpoint.clone(),
        execution_jwt: cmd.execution_jwt.clone(),
        pprof_bind_address: Some(cmd.pprof_addr.parse()?),
        suggested_fee_recipient: cmd.suggested_fee_recipient,
        skip_db_upgrade: cmd.skip_db_upgrade,
        rpc_sync_enabled: cmd.follow,
        rpc_sync_endpoints: cmd.follow_endpoints.clone(),
    };

    // Setup the application
    let app = App::new(config, args.get_home_dir()?, private_key_file, start_config);

    // Start the node
    rt.block_on(app.run())
}

fn init(args: &Args, cmd: &InitCmd, _logging: config::LoggingConfig) -> Result<()> {
    cmd.run(&args.get_default_priv_validator_key_file_path()?)
        .map_err(|error| eyre!("Failed to run init command {error:?}"))
}

fn key(args: &Args, cmd: &KeyCmd) -> Result<()> {
    cmd.run(&args.get_default_priv_validator_key_file_path()?)
        .map_err(|error| eyre!("Failed to run key command {error:?}"))
}

fn db_migrate(args: &Args, cmd: &MigrateCmd) -> Result<()> {
    info!("Starting database migration");

    let db_path = args.get_db_path()?;

    if !db_path.exists() {
        return Err(eyre!(
            "Database file does not exist at path: {}",
            db_path.display()
        ));
    }

    info!(path = %db_path.display(), "Opening database");

    let db = redb::Database::builder()
        .open(&db_path)
        .map_err(|e| eyre!("Failed to open database: {e}"))?;

    let coordinator = MigrationCoordinator::new(db);

    // Check if migration is needed
    let needs_migration = coordinator
        .needs_migration(db_path.exists())
        .map_err(|e| eyre!("Failed to check migration status: {e}"))?;

    if !needs_migration {
        info!("Database is already up to date");
        return Ok(());
    }

    if cmd.dry_run {
        info!("Dry-run mode: would perform migration but not committing");
        return Ok(());
    }

    info!("Performing database migration");

    // Perform migration
    let stats = coordinator
        .migrate()
        .map_err(|e| eyre!("Migration failed: {e}"))?;

    info!(
        tables = stats.tables_migrated,
        scanned = stats.records_scanned,
        upgraded = stats.records_upgraded,
        skipped = stats.records_skipped,
        duration = ?stats.duration,
        "Database migration completed successfully"
    );

    Ok(())
}

fn download(args: &Args, cmd: &DownloadCmd) -> Result<()> {
    let home_dir = args.get_home_dir()?;
    let rt = runtime::build_runtime(arc_consensus_types::RuntimeConfig::multi_threaded(0))?;
    rt.block_on(cmd.run(&home_dir))
}

fn compact(args: &Args) -> Result<()> {
    info!("Starting database compaction");

    let db_path = args.get_db_path()?;
    if !db_path.exists() {
        return Err(eyre!("Database file not found at {}", db_path.display()));
    }

    info!(path = %db_path.display(), "Opening database");

    let mut db = redb::Database::builder()
        .open(&db_path)
        .map_err(|e| eyre!("Failed to open database: {e}"))?;

    let before_size = std::fs::metadata(&db_path)
        .map_err(|e| eyre!("Failed to get database file metadata: {e}"))?
        .len();

    info!(size.before = %ByteSize::b(before_size), "Compacting database");

    db.compact()
        .map_err(|e| eyre!("Database compaction failed: {e}"))?;

    let after_size = std::fs::metadata(&db_path)
        .map_err(|e| eyre!("Failed to get database file metadata: {e}"))?
        .len();

    info!(
        size.before = %ByteSize::b(before_size),
        size.after = %ByteSize::b(after_size),
        reclaimed = %ByteSize::b(before_size.saturating_sub(after_size)),
        "Database compaction completed successfully"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_consensus_types::{LogFormat, LogLevel};
    use arc_node_consensus_cli::cmd::start::StartCmd;

    fn test_logging_config() -> config::LoggingConfig {
        config::LoggingConfig {
            log_level: LogLevel::Info,
            log_format: LogFormat::Plaintext,
        }
    }

    fn minimal_start_cmd() -> StartCmd {
        StartCmd {
            moniker: Some("test-node".to_string()),
            p2p_addr: "/ip4/127.0.0.1/tcp/27000".parse().unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn build_signing_config_defaults_to_local() {
        let cmd = minimal_start_cmd();
        let config = build_signing_config(&cmd).unwrap();
        assert!(matches!(config, SigningConfig::Local));
    }

    #[test]
    fn build_signing_config_with_remote_endpoint_only() {
        let mut cmd = minimal_start_cmd();
        cmd.signing_remote = Some("http://signer:10340".to_string());

        let config = build_signing_config(&cmd).unwrap();

        match config {
            SigningConfig::Remote(remote) => {
                assert_eq!(remote.endpoint, "http://signer:10340");
                assert_eq!(remote.timeout, std::time::Duration::from_secs(30));
                assert!(!remote.enable_tls);
                assert_eq!(remote.tls_cert_path, None);
            }
            _ => panic!("Expected remote signing config"),
        }
    }

    #[test]
    fn build_signing_config_with_remote_and_tls_cert() {
        let mut cmd = minimal_start_cmd();
        cmd.signing_remote = Some("http://signer:10340".to_string());
        cmd.signing_tls_cert_path = Some("/path/to/cert.pem".to_string());

        let config = build_signing_config(&cmd).unwrap();

        match config {
            SigningConfig::Remote(remote) => {
                assert_eq!(remote.endpoint, "http://signer:10340");
                assert_eq!(remote.timeout, std::time::Duration::from_secs(30));
                assert!(remote.enable_tls); // Auto-enabled when cert path provided
                assert_eq!(remote.tls_cert_path, Some("/path/to/cert.pem".to_string()));
            }
            _ => panic!("Expected remote signing config"),
        }
    }

    #[test]
    fn build_config_from_cli_with_minimal_flags() {
        let cmd = minimal_start_cmd();
        let logging = test_logging_config();

        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert_eq!(config.moniker, "test-node");
        assert!(config.consensus.enabled);
        assert_eq!(
            config.consensus.p2p.listen_addr.to_string(),
            "/ip4/127.0.0.1/tcp/27000"
        );
        assert!(config.consensus.p2p.persistent_peers.is_empty());
        assert!(!config.consensus.p2p.discovery.enabled);
        assert!(config.value_sync.enabled);
        assert!(!config.metrics.enabled);
        assert!(!config.rpc.enabled);
        assert_eq!(config.prune.certificates_distance, 0);
        assert!(matches!(config.signing, SigningConfig::Local));
    }

    #[test]
    fn build_config_from_cli_with_all_optional_flags() {
        let mut cmd = minimal_start_cmd();
        cmd.moniker = Some("validator-1".to_string());
        cmd.p2p_addr = "/ip4/172.19.0.5/tcp/27000".parse().unwrap();
        cmd.p2p_persistent_peers = vec![
            "/ip4/172.19.0.6/tcp/27000".parse().unwrap(),
            "/ip4/172.19.0.7/tcp/27000".parse().unwrap(),
        ];
        cmd.discovery = true;
        cmd.discovery_num_outbound_peers = 30;
        cmd.discovery_num_inbound_peers = 40;
        cmd.value_sync = false;
        cmd.metrics = Some("0.0.0.0:29000".parse().unwrap());
        cmd.rpc_addr = Some("0.0.0.0:31000".parse().unwrap());
        cmd.prune_certificates_distance = 1000;
        cmd.prune_certificates_before = 100;
        cmd.signing_remote = Some("http://signer:10340".to_string());

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert_eq!(config.moniker, "validator-1");
        assert_eq!(config.consensus.p2p.persistent_peers.len(), 2);
        assert!(config.consensus.p2p.discovery.enabled);
        assert_eq!(config.consensus.p2p.discovery.num_outbound_peers, 30);
        assert_eq!(config.consensus.p2p.discovery.num_inbound_peers, 40);
        assert!(!config.value_sync.enabled);
        assert!(config.metrics.enabled);
        assert_eq!(config.metrics.listen_addr.to_string(), "0.0.0.0:29000");
        assert!(config.rpc.enabled);
        assert_eq!(config.rpc.listen_addr.to_string(), "0.0.0.0:31000");
        assert_eq!(config.prune.certificates_distance, 1000);
        assert_eq!(config.prune.certificates_before.as_u64(), 100);
        assert!(matches!(config.signing, SigningConfig::Remote(_)));
    }

    #[test]
    fn build_config_from_cli_persistent_peers_only() {
        let mut cmd = minimal_start_cmd();
        cmd.p2p_persistent_peers_only = true;

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(config.consensus.p2p.persistent_peers_only);
    }

    #[test]
    fn build_config_from_cli_persistent_peers_only_default_false() {
        let cmd = minimal_start_cmd();
        let logging = test_logging_config();

        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(!config.consensus.p2p.persistent_peers_only);
    }

    #[test]
    fn build_config_from_cli_calculates_num_nodes_from_peers() {
        let mut cmd = minimal_start_cmd();
        cmd.p2p_persistent_peers = vec![
            "/ip4/172.19.0.6/tcp/27000".parse().unwrap(),
            "/ip4/172.19.0.7/tcp/27000".parse().unwrap(),
            "/ip4/172.19.0.8/tcp/27000".parse().unwrap(),
        ];

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        // num_nodes = persistent_peers.len() + 1 = 3 + 1 = 4
        // This affects gossipsub config generation
        assert_eq!(config.consensus.p2p.persistent_peers.len(), 3);
    }

    #[test]
    fn build_config_from_cli_metrics_enabled_when_flag_present() {
        let mut cmd = minimal_start_cmd();
        cmd.metrics = Some("127.0.0.1:29000".parse().unwrap());

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(config.metrics.enabled);
        assert_eq!(config.metrics.listen_addr.to_string(), "127.0.0.1:29000");
    }

    #[test]
    fn build_config_from_cli_metrics_disabled_when_flag_absent() {
        let cmd = minimal_start_cmd();

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(!config.metrics.enabled);
        // Default address is still set even when disabled
        assert_eq!(config.metrics.listen_addr.to_string(), "0.0.0.0:29000");
    }

    #[test]
    fn build_config_from_cli_rpc_enabled_when_flag_present() {
        let mut cmd = minimal_start_cmd();
        cmd.rpc_addr = Some("127.0.0.1:31000".parse().unwrap());

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(config.rpc.enabled);
        assert_eq!(config.rpc.listen_addr.to_string(), "127.0.0.1:31000");
    }

    #[test]
    fn build_config_from_cli_rpc_disabled_when_flag_absent() {
        let cmd = minimal_start_cmd();

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert!(!config.rpc.enabled);
        // Default address is still set even when disabled
        assert_eq!(config.rpc.listen_addr.to_string(), "0.0.0.0:31000");
    }

    #[test]
    fn build_config_from_cli_pruning_configuration() {
        let mut cmd = minimal_start_cmd();
        cmd.prune_certificates_distance = 500;
        cmd.prune_certificates_before = 50;

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert_eq!(config.prune.certificates_distance, 500);
        assert_eq!(config.prune.certificates_before.as_u64(), 50);
    }

    #[test]
    fn build_config_from_cli_full_preset() {
        let mut cmd = minimal_start_cmd();
        cmd.full = true;

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert_eq!(
            config.prune.certificates_distance,
            PRESETS_PRUNE_CERTIFICATES_DISTANCE
        );
        assert_eq!(config.prune.certificates_before.as_u64(), 0);
    }

    #[test]
    fn build_config_from_cli_minimal_preset() {
        let mut cmd = minimal_start_cmd();
        cmd.minimal = true;

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        assert_eq!(
            config.prune.certificates_distance,
            PRESETS_PRUNE_CERTIFICATES_DISTANCE
        );
        assert_eq!(config.prune.certificates_before.as_u64(), 0);
    }

    #[test]
    fn build_config_from_cli_uses_hardcoded_runtime() {
        let cmd = minimal_start_cmd();
        let logging = test_logging_config();

        let config = build_config_from_cli(&cmd, logging).unwrap();

        // Should use the multi-threaded runtime
        assert_eq!(config.runtime, RuntimeConfig::multi_threaded(0));
    }

    #[test]
    fn build_config_from_cli_multi_threaded_runtime_with_threads() {
        let mut cmd = minimal_start_cmd();
        cmd.runtime_flavor = "multi-threaded".to_string();
        cmd.worker_threads = Some(8);

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        // Should use the multi-threaded runtime with 8 threads
        assert_eq!(config.runtime, RuntimeConfig::multi_threaded(8));
    }

    #[test]
    fn build_config_from_cli_single_threaded_runtime() {
        let mut cmd = minimal_start_cmd();
        cmd.runtime_flavor = "single-threaded".to_string();

        let logging = test_logging_config();
        let config = build_config_from_cli(&cmd, logging).unwrap();

        // Should use the single-threaded runtime
        assert_eq!(config.runtime, RuntimeConfig::single_threaded());
    }

    #[test]
    fn build_config_from_cli_invalid_runtime_flavor() {
        let mut cmd = minimal_start_cmd();
        cmd.runtime_flavor = "invalid-flavor".to_string();

        let logging = test_logging_config();
        let result = build_config_from_cli(&cmd, logging);

        assert!(result.is_err());
        let error_message = format!("{}", result.unwrap_err());
        assert!(error_message.contains("Invalid runtime flavor"));
    }
}
