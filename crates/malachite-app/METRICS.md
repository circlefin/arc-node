# Malachite App Metrics

This application exposes Prometheus metrics on the `/metrics` endpoint. The following is a list of key metrics and their descriptions.

## Metrics

### Application Metrics

- **`arc_malachite_app_block_time`** (Histogram)
  - **Description:** Interval between two blocks, in seconds.
  - **Buckets:** Provides a histogram of block intervals with exponential buckets from 0.01 to 2.0 seconds.

- **`arc_malachite_app_block_finalize_time`** (Histogram)
  - **Description:** Time taken to finalize a block, in seconds.
  - **Buckets:** Provides a histogram of block finalization times with exponential buckets from 0.01 to 2.0 seconds.

- **`arc_malachite_app_block_build_time`** (Histogram)
  - **Description:** Time taken by the proposer to build a block, in seconds.
  - **Buckets:** Provides a histogram of block build times with exponential buckets from 0.01 to 2.0 seconds.

- **`arc_malachite_app_block_transactions_count`** (Histogram)
  - **Description:** Number of transactions in each finalized block.
  - **Buckets:** Exponential buckets: 1, 2, 4, .. , 8192, 16384 (16K) transactions.

- **`arc_malachite_app_block_size_bytes`** (Histogram)
  - **Description:** Size of each finalized block in bytes.
  - **Buckets:** Exponential buckets: 1KB, 2KB, 4KB, .. , 512MB, 1GB.

- **`arc_malachite_app_block_gas_used`** (Histogram)
  - **Description:** Gas used in each finalized block.
  - **Buckets:** Exponential buckets: 1K, 2K, 4K, .. , 16M, 32M gas.

- **`arc_malachite_app_total_transactions_count`** (Counter)
  - **Description:** Total number of transactions finalized since the application started.

- **`arc_malachite_app_total_chain_bytes`** (Counter)
  - **Description:** Total size of all finalized block payloads in bytes since the application started.

- **`arc_malachite_app_validators_count`** (Gauge)
  - **Description:** Number of validators in the current validator set.

- **`arc_malachite_app_validators_total_voting_power`** (Gauge)
  - **Description:** Total voting power of the current validator set.

- **`arc_malachite_app_validator_voting_power`** (Gauge)
  - **Description:** Voting power of each validator.
  - **Labels:**
    - `address`: The validator's address.

- **`arc_malachite_app_consensus_params`** (Gauge)
  - **Description:** Current consensus parameters.
  - **Labels:**
    - `timeout_propose`: Timeout for proposing a block (seconds)
    - `timeout_propose_delta`: By which amount the timeout for proposing a block can be increased (seconds)
    - `timeout_prevote`: Timeout for voting on a block (seconds)
    - `timeout_prevote_delta`: By which amount the timeout for voting on a block can be increased (seconds)
    - `timeout_precommit`: Timeout for committing a block (seconds)
    - `timeout_precommit_delta`: By which amount the timeout for committing a block can be increased (seconds)
    - `timeout_rebroadcast`: Timeout for rebroadcasting a block (seconds)
    - `target_block_time`: The target block time, or `0` if no target block time is set (seconds)

- **`arc_malachite_app_msg_process_time`** (Histogram)
  - **Description:** Time taken to process a message, in seconds.
  - **Labels:**
    - `msg`: The type of message being processed.
  - **Buckets:** Provides a histogram of message processing times with exponential buckets from 0.01 to 2.0 seconds.

- **`arc_malachite_app_engine_api_time`** (Histogram)
  - **Description:** Time taken for each Engine API call, in seconds.
  - **Labels:**
    - `api`: The Engine API method name.
  - **Buckets:** Provides a histogram of API call times with exponential buckets from 0.001 to 2.0 seconds.

- **`arc_malachite_app_height_restart_count`** (Counter)
  - **Description:** Number of times the consensus height has been restarted due to errors or recovery scenarios.

- **`arc_malachite_app_sync_fell_behind_count`** (Counter)
  - **Description:** Number of times the node fell behind and transitioned from InSync to CatchingUp.

- **`arc_malachite_app_pending_proposal_parts_count`** (Gauge)
  - **Description:** Number of pending proposal parts waiting to be processed at a future height or round.

- **`arc_malachite_app_version_info`** (Info)
  - **Description:** Version information for the consensus layer.
  - **Labels:**
    - `version`: Version string in format "v0.2.0-rc1 (3ecc9383)" combining git tag and short commit hash.
    - `git_commit`: Full git commit hash (40 characters).

### Database Metrics

- **`arc_malachite_app_db_size`** (Gauge)
  - **Description:** Size of the database in bytes.

- **`arc_malachite_app_db_write_bytes`** (Counter)
  - **Description:** Amount of data written to the database in bytes.

- **`arc_malachite_app_db_read_bytes`** (Counter)
  - **Description:** Amount of data read from the database in bytes.

- **`arc_malachite_app_db_key_read_bytes`** (Counter)
  - **Description:** Amount of key data read from the database in bytes.

- **`arc_malachite_app_db_read_count`** (Counter)
  - **Description:** Total number of reads from the database.

- **`arc_malachite_app_db_write_count`** (Counter)
  - **Description:** Total number of writes to the database.

- **`arc_malachite_app_db_delete_count`** (Counter)
  - **Description:** Total number of deletions from the database.

- **`arc_malachite_app_db_read_time`** (Histogram)
  - **Description:** Time taken to read bytes from the database in seconds.
  - **Buckets:** Provides a histogram of read times with exponential buckets from 0.001 to 2.0 seconds.

- **`arc_malachite_app_db_write_time`** (Histogram)
  - **Description:** Time taken to write bytes to the database in seconds.
  - **Buckets:** Provides a histogram of write times with exponential buckets from 0.001 to 2.0 seconds.

- **`arc_malachite_app_db_delete_time`** (Histogram)
  - **Description:** Time taken to delete bytes from the database in seconds.
  - **Buckets:** Provides a histogram of delete times with exponential buckets from 0.001 to 2.0 seconds.

### Remote Signer Metrics

- **`arc_remote_signer_sign_requests_count`** (Counter)
  - **Description:** Total number of sign requests received.
- **`arc_remote_signer_sign_request_errors`** (Counter)
  - **Description:** Total number of sign request errors.
- **`arc_remote_signer_sign_request_retries`** (Counter)
  - **Description:** Total number of sign request retries.
- **`arc_remote_signer_sign_request_latency_total`** (Histogram)
  - **Description:** Latency of sign requests in seconds (including retries).
  - **Buckets:** Provides a histogram of latency distributions with exponential buckets from 1ms to 10s.
- **`arc_remote_signer_sign_request_latency_single`** (Histogram)
  - **Description:** Latency of sign requests in seconds (excluding retries).
  - **Buckets:** Provides a histogram of latency distributions with exponential buckets from 1ms to 10s.


### Jemalloc Memory Metrics (Unix systems)

- **`arc_malachite_app_jemalloc_active`** (Gauge)
  - **Description:** Total number of bytes in active pages allocated by the application.

- **`arc_malachite_app_jemalloc_allocated`** (Gauge)
  - **Description:** Total number of bytes allocated by the application.

- **`arc_malachite_app_jemalloc_mapped`** (Gauge)
  - **Description:** Total number of bytes in active extents mapped by the allocator.

- **`arc_malachite_app_jemalloc_metadata`** (Gauge)
  - **Description:** Total number of bytes dedicated to jemalloc metadata.

- **`arc_malachite_app_jemalloc_resident`** (Gauge)
  - **Description:** Total number of bytes in physically resident data pages mapped by the allocator.

- **`arc_malachite_app_jemalloc_retained`** (Gauge)
  - **Description:** Total number of bytes in virtual memory mappings that were retained rather than being returned to the operating system.

### IO And Stats Metrics (Linux only)

- **`arc_malachite_app_io_rchar`** (Gauge)
  - **Description:** Characters read.

- **`arc_malachite_app_io_wchar`** (Gauge)
  - **Description:** Characters written.

- **`arc_malachite_app_io_syscr`** (Gauge)
  - **Description:** Read syscalls.

- **`arc_malachite_app_io_syscw`** (Gauge)
  - **Description:** Write syscalls.

- **`arc_malachite_app_io_read_bytes`** (Gauge)
  - **Description:** Bytes read.

- **`arc_malachite_app_io_write_bytes`** (Gauge)
  - **Description:** Bytes written.

- **`arc_malachite_app_io_cancelled_write_bytes`** (Gauge)
  - **Description:** Cancelled write bytes.

- **`arc_malachite_app_process_cpu_seconds_total`** (Gauge)
  - **Description:** Total user and system CPU time spent in seconds.

- **`arc_malachite_app_process_open_fds`** (Gauge)
  - **Description:** Number of open file descriptors.

- **`arc_malachite_app_process_threads`** (Gauge)
  - **Description:** Number of OS threads in the process.

## Feature Requirements

### Platform Support
- **Jemalloc Metrics**: Available on Unix systems
- **IO And Stats Metrics**: Available on Linux only
