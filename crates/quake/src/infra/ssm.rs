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

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::{eyre, Context, Result};
use rand::random;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::infra::{remote, NodeInfraData};
use crate::shell;
use crate::util::in_parallel;

/// Local file that stores the SSM owner ID for one imported remote testnet.
pub(crate) const OWNER_ID_FILENAME: &str = ".ssm-owner-id";

/// Each developer needs a stable local owner ID so Quake only matches
/// and terminates the SSM tunnels created by this machine.
pub(crate) fn ensure_owner_id(testnet_dir: &Path) -> Result<String> {
    let owner_id_path = testnet_dir.join(OWNER_ID_FILENAME);
    if owner_id_path.exists() {
        return load_owner_id(testnet_dir);
    }

    let owner_id = format!("{:016x}", random::<u64>());
    fs::write(&owner_id_path, &owner_id).with_context(|| {
        format!(
            "Failed to write local SSM owner ID to {}",
            owner_id_path.display()
        )
    })?;
    Ok(owner_id)
}

/// Quake must reuse the same owner ID on one machine, otherwise it
/// loses track of its own tunnels and treats them as foreign.
pub(crate) fn load_owner_id(testnet_dir: &Path) -> Result<String> {
    let owner_id_path = testnet_dir.join(OWNER_ID_FILENAME);
    let owner_id = fs::read_to_string(&owner_id_path).with_context(|| {
        format!(
            "Failed to read local SSM owner ID from {}",
            owner_id_path.display()
        )
    })?;
    validate_owner_id(owner_id.trim())
        .with_context(|| format!("Invalid SSM owner ID in {}", owner_id_path.display()))
}

/// Ensures `owner_id` is exactly 16 lowercase hex characters.
fn validate_owner_id(owner_id: &str) -> Result<String> {
    let is_lower_hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    if owner_id.len() != 16 || !owner_id.bytes().all(is_lower_hex) {
        return Err(eyre!("expected a 16-character lowercase hex owner ID"));
    }

    Ok(owner_id.to_string())
}

/// [`Ssm`] manages the local SSM tunnels to Control Center.
///
/// Remote Quake commands expect a small set of localhost ports to forward into
/// Control Center. If those local forwards disappear, an imported testnet
/// loses access to Grafana, Prometheus, and the other Control Center
/// endpoints.
///
/// This type exists to keep those tunnels in one place. Other code can ask it
/// to start them, stop them, or print their current state.
#[derive(Clone)]
pub(crate) struct Ssm {
    sessions: Vec<SsmSession>,
    backend: Arc<dyn SsmBackend>,
}

impl Ssm {
    /// `cc` is optional because Quake can build remote testnet state before
    /// Terraform has created the Control Center.
    pub fn new(owner_id: String, cc: Option<&NodeInfraData>) -> Result<Self> {
        let mut ssm_sessions = Vec::new();

        // `None` means remote infra has not been provisioned yet, so there is
        // no Control Center and no tunnels to build.
        if let Some(cc) = cc {
            let owner_id = validate_owner_id(&owner_id)
                .wrap_err("Invalid local SSM owner ID for SSM tunnels")?;
            if let Some(ssm_tunnel_ports) = cc.ssm_tunnel_ports.as_ref() {
                let instance_id = cc.instance_id().wrap_err("Instance ID not found for CC")?;
                for (remote_port, local_port) in ssm_tunnel_ports.iter() {
                    ssm_sessions.push(SsmSession::new(
                        remote::CC_INSTANCE.to_string(),
                        instance_id.clone(),
                        owner_id.clone(),
                        *local_port,
                        *remote_port,
                    ));
                }
            }
        }

        Ok(Self {
            sessions: ssm_sessions,
            backend: Arc::new(AwsCliBackend),
        })
    }

    /// A tunnel is only usable when the local port is listening *and* AWS
    /// has the matching session. If another local process owns one of
    /// Quake's expected ports, this fails rather than treating it as
    /// healthy.
    pub async fn start(&self) -> Result<()> {
        debug!("Starting SSM sessions");

        let aws_sessions = self.managed_aws_sessions()?;
        let mut conflicting_sessions = Vec::new();
        let mut stale_sessions = Vec::new();
        let mut sessions_to_start = Vec::new();

        for session in &self.sessions {
            let aws_sessions_for_tunnel = self.aws_sessions_for_tunnel(session, &aws_sessions);
            let local_listener = self.backend.is_local_port_listening(session.local_port);
            let status = self.tunnel_status(local_listener, aws_sessions_for_tunnel.len());
            match status {
                TunnelStatus::Usable => continue,
                TunnelStatus::Conflict => {
                    conflicting_sessions.push(session.label());
                }
                TunnelStatus::StaleAws => {
                    stale_sessions.extend(aws_sessions_for_tunnel.into_iter().cloned());
                    sessions_to_start.push(session);
                }
                TunnelStatus::Missing => sessions_to_start.push(session),
            }
        }

        if !conflicting_sessions.is_empty() {
            let conflicts = conflicting_sessions.join(", ");
            return Err(eyre!(
                "Quake SSM localhost ports are already occupied by another local process: {conflicts}"
            ));
        }

        self.terminate_sessions(&stale_sessions).await?;
        self.start_sessions(&sessions_to_start).await?;
        self.wait_for_local_listeners(&sessions_to_start, Duration::from_secs(60))
            .await?;

        info!("✅ SSM sessions ready");
        Ok(())
    }

    /// Without cleanup, AWS can still show this machine's old sessions for
    /// tunnels that no longer help the user.
    pub async fn stop(&self) -> Result<()> {
        debug!("Closing SSM sessions");

        let aws_sessions = self.managed_aws_sessions()?;
        self.terminate_sessions(&aws_sessions).await?;

        info!("✅ SSM sessions terminated");
        Ok(())
    }

    /// Shows Quake's local view of each tunnel and AWS's view of this
    /// machine's matching sessions.
    pub async fn list(&self) -> Result<()> {
        println!("{}", self.list_formatted()?);
        Ok(())
    }

    /// Builds the text for `quake remote ssm list`.
    pub fn list_formatted(&self) -> Result<String> {
        if self.sessions.is_empty() {
            return Ok("SSM tunnels:\n  - No configured SSM tunnels\n".to_string());
        }

        let aws_sessions = self.managed_aws_sessions()?;
        let lines = self
            .sessions
            .iter()
            .map(|ssm_session| {
                let aws_sessions_for_tunnel =
                    self.aws_sessions_for_tunnel(ssm_session, &aws_sessions);
                let local_listener = self.backend.is_local_port_listening(ssm_session.local_port);
                let status = self.tunnel_status(local_listener, aws_sessions_for_tunnel.len());

                let aws_details = Self::format_matching_aws_sessions(&aws_sessions_for_tunnel);

                let listener_str = if local_listener { "up" } else { "down" };
                format!(
                    "  - localhost:{} -> {}:{} status: {} listener: {} aws_sessions: {} aws: {}",
                    ssm_session.local_port,
                    ssm_session.instance_name,
                    ssm_session.remote_port,
                    status,
                    listener_str,
                    aws_sessions_for_tunnel.len(),
                    aws_details,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(format!(
            "SSM tunnels (local -> remote, status, local listener, \
matching AWS sessions, AWS details):\n{lines}\n"
        ))
    }

    /// Filters out foreign sessions (other developers) and unrelated
    /// sessions (e.g. direct SSH to Control Center).
    fn managed_aws_sessions(&self) -> Result<Vec<AwsSession>> {
        let expected_reasons = self
            .sessions
            .iter()
            .map(SsmSession::reason)
            .collect::<HashSet<_>>();

        Ok(self
            .backend
            .aws_sessions(&self.sessions)?
            .into_iter()
            .filter(|session| expected_reasons.contains(&session.reason))
            .collect())
    }

    /// Matches AWS sessions to one expected tunnel by `reason` string.
    fn aws_sessions_for_tunnel<'a>(
        &self,
        session: &SsmSession,
        aws_sessions: &'a [AwsSession],
    ) -> Vec<&'a AwsSession> {
        aws_sessions
            .iter()
            .filter(|active| active.reason == session.reason())
            .collect()
    }

    /// Formats AWS session IDs and statuses for `quake remote ssm list`.
    fn format_matching_aws_sessions(sessions: &[&AwsSession]) -> String {
        if sessions.is_empty() {
            return "[]".to_string();
        }

        let mut details = sessions
            .iter()
            .map(|session| format!("{}:{}", session.session_id, session.status))
            .collect::<Vec<_>>();
        details.sort();

        format!("[{}]", details.join(", "))
    }

    /// Derives the tunnel health from the local port check and matching
    /// AWS session count.
    fn tunnel_status(&self, local_listener: bool, aws_sessions_count: usize) -> TunnelStatus {
        match (local_listener, aws_sessions_count > 0) {
            (true, true) => TunnelStatus::Usable,
            (true, false) => TunnelStatus::Conflict,
            (false, true) => TunnelStatus::StaleAws,
            (false, false) => TunnelStatus::Missing,
        }
    }

    /// Starts missing tunnels in parallel.
    async fn start_sessions(&self, sessions: &[&SsmSession]) -> Result<()> {
        if sessions.is_empty() {
            return Ok(());
        }

        let start_results = in_parallel(sessions, move |session| {
            let backend = Arc::clone(&self.backend);
            async move { backend.start_session(&session) }
        })
        .await;

        for (session, result) in sessions.iter().zip(start_results) {
            if let Err(e) = result {
                return Err(eyre!("Failed to start SSM tunnel {}: {e}", session.label()));
            }
        }
        Ok(())
    }

    /// Removes stale AWS sessions before starting fresh tunnels.
    async fn terminate_sessions(&self, sessions: &[AwsSession]) -> Result<()> {
        if sessions.is_empty() {
            return Ok(());
        }

        let session_refs = sessions.iter().collect::<Vec<_>>();
        let stop_results = in_parallel(&session_refs, move |session| {
            let backend = Arc::clone(&self.backend);
            async move { backend.terminate_session(&session.session_id) }
        })
        .await;

        for (session, result) in sessions.iter().zip(stop_results) {
            if let Err(e) = result {
                return Err(eyre!(
                    "Failed to stop SSM session {}: {e}",
                    session.session_id
                ));
            }
        }
        Ok(())
    }

    /// Polls until all newly started local ports accept TCP connections.
    async fn wait_for_local_listeners(
        &self,
        sessions: &[&SsmSession],
        timeout: Duration,
    ) -> Result<()> {
        let start_time = Instant::now();
        while start_time.elapsed() < timeout {
            if self.all_local_ports_listening(sessions) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(eyre!(
            "Timeout waiting for SSM tunnels to open local listeners"
        ))
    }

    /// checks whether every expected local forwarded port is accepting connections.
    fn all_local_ports_listening(&self, sessions: &[&SsmSession]) -> bool {
        sessions
            .iter()
            .all(|session| self.backend.is_local_port_listening(session.local_port))
    }
}

/// The pieces of an AWS SSM session that Quake needs for matching,
/// cleanup, and reporting.
#[derive(Clone)]
struct AwsSession {
    session_id: String,
    /// AWS status word (e.g. `Connected`), shown in `ssm list`.
    status: String,
    /// Quake's stable tunnel name, used to match AWS sessions back to
    /// expected tunnels across runs.
    reason: String,
}

/// Quake's tunnel-health classification.
enum TunnelStatus {
    /// Local port listening, AWS session present.
    Usable,
    /// Local port listening, but no matching AWS session — another process
    /// owns the port.
    Conflict,
    /// AWS session present, but local port not listening — stale record.
    StaleAws,
    /// Neither local port nor AWS session exists.
    Missing,
}

impl fmt::Display for TunnelStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Usable => "usable",
            Self::Conflict => "conflict",
            Self::StaleAws => "stale_aws",
            Self::Missing => "missing",
        };

        f.write_str(label)
    }
}

/// Seam between [`Ssm`] logic and external side effects (AWS CLI, TCP
/// probes). Tests replace the real backend with a mock.
trait SsmBackend: Send + Sync {
    /// Lists active AWS SSM sessions for the given expected tunnels.
    fn aws_sessions(&self, sessions: &[SsmSession]) -> Result<Vec<AwsSession>>;
    /// Returns `true` if `local_port` on localhost accepts a TCP connection.
    fn is_local_port_listening(&self, local_port: u16) -> bool;
    /// Starts one SSM port-forwarding tunnel in the background.
    fn start_session(&self, session: &SsmSession) -> Result<()>;
    /// Terminates one AWS SSM session by ID.
    fn terminate_session(&self, session_id: &str) -> Result<()>;
}

/// Real [`SsmBackend`] that shells out to the AWS CLI.
struct AwsCliBackend;

impl SsmBackend for AwsCliBackend {
    fn aws_sessions(&self, sessions: &[SsmSession]) -> Result<Vec<AwsSession>> {
        if sessions.is_empty() {
            return Ok(Vec::new());
        }

        // currently all sessions share the same instance ID, so we can just use the
        // first. If that ever changes, we'll need to query sessions for each
        // instance separately.
        let instance_id = sessions[0].instance_id.as_str();
        let query = format!(
            "Sessions[?Target==`{instance_id}`] | sort_by([], \
&to_string(Reason))[*] | [].[SessionId, Status, Reason]"
        );

        #[rustfmt::skip]
        let args = vec![
            "ssm", "describe-sessions",
            "--state", "Active",
            "--output", "text",
            "--query", &query,
        ];

        let result = shell::exec_with_output("aws", args, Path::new("."))
            .wrap_err("Failed to query active SSM sessions")?;

        Ok(parse_aws_sessions_output(&result))
    }

    fn is_local_port_listening(&self, local_port: u16) -> bool {
        is_local_port_listening(local_port)
    }

    fn start_session(&self, session: &SsmSession) -> Result<()> {
        debug!(
            instance_id = %session.instance_id,
            local_port = session.local_port,
            remote_port = session.remote_port,
            "Starting SSM tunnel"
        );

        let reason = session.reason();
        let instance_id = session.instance_id.to_owned();
        let local_port = session.local_port;
        let remote_port = session.remote_port;
        tokio::spawn(async move {
            #[rustfmt::skip]
            let args = [
                "ssm", "start-session",
                "--target", &instance_id,
                "--reason", &reason,
                "--document-name", "AWS-StartPortForwardingSession",
                "--parameters", &format!(
                    "{{\"portNumber\":[\"{remote_port}\"],\
\"localPortNumber\":[\"{local_port}\"]}}"
                ),
            ];
            if let Err(e) = shell::exec("aws", args.to_vec(), Path::new("."), None, true) {
                warn!(
                    instance_id = %instance_id,
                    local_port,
                    remote_port,
                    "Failed to start SSM tunnel: {e}"
                );
            }
        });
        Ok(())
    }

    fn terminate_session(&self, session_id: &str) -> Result<()> {
        let args = ["ssm", "terminate-session", "--session-id", session_id];
        shell::exec("aws", args.to_vec(), Path::new("."), None, false)
    }
}

/// One expected tunnel: instance name + ID, local port, remote port.
#[derive(Clone)]
pub(crate) struct SsmSession {
    /// The short name Quake shows for the target machine.
    instance_name: String,
    /// The EC2 instance that the tunnel connects to.
    instance_id: String,
    /// Isolates this machine's tunnels from other developers'.
    owner_id: String,
    local_port: u16,
    remote_port: u16,
}

impl SsmSession {
    pub fn new(
        instance_name: String,
        instance_id: String,
        owner_id: String,
        local_port: u16,
        remote_port: u16,
    ) -> Self {
        Self {
            instance_name,
            instance_id,
            owner_id,
            local_port,
            remote_port,
        }
    }

    /// Stable name written into AWS `reason` so later runs on this machine can
    /// match sessions back to expected tunnels.
    fn reason(&self) -> String {
        format!(
            "quake-{}-{}-owner{}-{}-{}",
            self.instance_name, self.instance_id, self.owner_id, self.local_port, self.remote_port
        )
    }

    fn label(&self) -> String {
        format!(
            "localhost:{} -> {}:{}",
            self.local_port, self.instance_name, self.remote_port
        )
    }
}

/// Parses `aws ssm describe-sessions --output text` into [`AwsSession`]s.
fn parse_aws_sessions_output(output: &str) -> Vec<AwsSession> {
    output
        .lines()
        .filter_map(|line| {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() < 3 {
                return None;
            }

            Some(AwsSession {
                session_id: parts[0].to_string(),
                status: parts[1].to_string(),
                reason: parts[2].to_string(),
            })
        })
        .collect()
}

/// Checks whether a localhost port accepts a TCP connection.
fn is_local_port_listening(local_port: u16) -> bool {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, local_port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
}

#[cfg(test)]
impl Ssm {
    /// Builds an [`Ssm`] with a mock backend for tests.
    fn with_backend(sessions: Vec<SsmSession>, backend: Arc<dyn SsmBackend>) -> Self {
        Self { sessions, backend }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::TcpListener;
    use std::sync::Mutex;
    use tempfile::tempdir;

    const TEST_OWNER_ID: &str = "8f3c12a1b7d94e6c";
    const FOREIGN_OWNER_ID: &str = "71de44b9538a0c2f";

    #[derive(Default)]
    struct MockSsmBackend {
        aws_sessions: Mutex<Vec<AwsSession>>,
        listening_ports: Mutex<HashSet<u16>>,
        started_reasons: Mutex<Vec<String>>,
        terminated_sessions: Mutex<Vec<String>>,
        /// `next_id` keeps the counter used to mint fake AWS session IDs.
        next_id: Mutex<usize>,
    }

    impl MockSsmBackend {
        /// Tests use this to create stale-session cases, where AWS still
        /// shows a session after the local tunnel is gone.
        fn add_aws_session(&self, session_id: &str, reason: String) {
            self.aws_sessions
                .lock()
                .expect("aws_sessions mutex poisoned")
                .push(AwsSession {
                    session_id: session_id.to_string(),
                    status: "Connected".to_string(),
                    reason,
                });
        }

        /// Tests use this to say, "this tunnel is healthy," without starting
        /// a real background session.
        fn listen_on(&self, local_port: u16) {
            self.listening_ports
                .lock()
                .expect("listening_ports mutex poisoned")
                .insert(local_port);
        }

        /// The tests check the `reason` strings because that is how Quake
        /// names tunnels across runs.
        fn started_reasons(&self) -> Vec<String> {
            self.started_reasons
                .lock()
                .expect("started_reasons mutex poisoned")
                .clone()
        }

        /// The tests use this to verify that stale AWS sessions are cleaned up
        /// before new tunnels are started.
        fn terminated_sessions(&self) -> Vec<String> {
            self.terminated_sessions
                .lock()
                .expect("terminated_sessions mutex poisoned")
                .clone()
        }
    }

    impl SsmBackend for MockSsmBackend {
        fn aws_sessions(&self, _sessions: &[SsmSession]) -> Result<Vec<AwsSession>> {
            Ok(self
                .aws_sessions
                .lock()
                .expect("aws_sessions mutex poisoned")
                .clone())
        }

        fn is_local_port_listening(&self, local_port: u16) -> bool {
            self.listening_ports
                .lock()
                .expect("listening_ports mutex poisoned")
                .contains(&local_port)
        }

        fn start_session(&self, session: &SsmSession) -> Result<()> {
            self.started_reasons
                .lock()
                .expect("started_reasons mutex poisoned")
                .push(session.reason());
            self.listen_on(session.local_port);

            let mut next_id = self.next_id.lock().expect("next_id mutex poisoned");
            *next_id += 1;
            self.add_aws_session(&format!("started-{}", *next_id), session.reason());
            Ok(())
        }

        fn terminate_session(&self, session_id: &str) -> Result<()> {
            self.terminated_sessions
                .lock()
                .expect("terminated_sessions mutex poisoned")
                .push(session_id.to_string());
            self.aws_sessions
                .lock()
                .expect("aws_sessions mutex poisoned")
                .retain(|session| session.session_id != session_id);
            Ok(())
        }
    }

    fn test_session(local_port: u16, remote_port: u16) -> SsmSession {
        SsmSession::new(
            remote::CC_INSTANCE.to_string(),
            "i-1234567890".to_string(),
            TEST_OWNER_ID.to_string(),
            local_port,
            remote_port,
        )
    }

    fn test_session_with_owner(owner_id: &str, local_port: u16, remote_port: u16) -> SsmSession {
        SsmSession::new(
            remote::CC_INSTANCE.to_string(),
            "i-1234567890".to_string(),
            owner_id.to_string(),
            local_port,
            remote_port,
        )
    }

    #[test]
    fn local_port_probe_detects_live_listener() {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral listener");
        let local_port = listener.local_addr().expect("listener local addr").port();

        assert!(is_local_port_listening(local_port));
    }

    #[test]
    fn parse_aws_sessions_output_parses_valid_rows() {
        let output = "\
session-1 Connected quake-cc-i-123-13000-3000
session-2 Terminated quake-cc-i-123-19090-9090
";

        let sessions = parse_aws_sessions_output(output);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(sessions[0].status, "Connected");
        assert_eq!(sessions[0].reason, "quake-cc-i-123-13000-3000");
        assert_eq!(sessions[1].session_id, "session-2");
        assert_eq!(sessions[1].status, "Terminated");
        assert_eq!(sessions[1].reason, "quake-cc-i-123-19090-9090");
    }

    #[test]
    fn parse_aws_sessions_output_skips_short_rows() {
        let output = "\
session-1 Connected quake-cc-i-123-13000-3000

too-short
session-2 Connected
session-3 Failed quake-cc-i-123-8000-80
";

        let sessions = parse_aws_sessions_output(output);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(sessions[0].status, "Connected");
        assert_eq!(sessions[0].reason, "quake-cc-i-123-13000-3000");
        assert_eq!(sessions[1].session_id, "session-3");
        assert_eq!(sessions[1].status, "Failed");
        assert_eq!(sessions[1].reason, "quake-cc-i-123-8000-80");
    }

    #[test]
    fn ensure_owner_id_creates_and_reuses_local_owner_id() {
        let dir = tempdir().expect("create tempdir");

        let first = ensure_owner_id(dir.path()).expect("owner ID created");
        let second = ensure_owner_id(dir.path()).expect("owner ID reused");

        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(first
            .bytes()
            .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) }));
        let stored =
            fs::read_to_string(dir.path().join(OWNER_ID_FILENAME)).expect("owner ID file exists");
        assert_eq!(stored, first);
    }

    #[test]
    fn load_owner_id_rejects_malformed_owner_id() {
        let dir = tempdir().expect("create tempdir");
        fs::write(dir.path().join(OWNER_ID_FILENAME), "not-a-valid-owner-id")
            .expect("write owner ID");

        let err = load_owner_id(dir.path()).expect_err("invalid owner ID should fail");

        assert!(err.to_string().contains("Invalid SSM owner ID"));
    }

    #[tokio::test]
    async fn start_skips_ports_that_are_already_listening() {
        let session = test_session(13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.listen_on(session.local_port);
        backend.add_aws_session("grafana-1", session.reason());
        let ssm = Ssm::with_backend(vec![session], backend.clone());

        ssm.start().await.expect("start succeeds");

        assert!(backend.started_reasons().is_empty());
        assert!(backend.terminated_sessions().is_empty());
    }

    /// Also proves `start` does not mutate state once it sees a conflict.
    #[tokio::test]
    async fn start_errors_when_listener_exists_without_matching_aws_session() {
        let conflict = test_session(13000, 3000);
        let stale = test_session(19090, 9090);
        let backend = Arc::new(MockSsmBackend::default());
        backend.listen_on(conflict.local_port);
        backend.add_aws_session("prom-1", stale.reason());
        let ssm = Ssm::with_backend(vec![conflict, stale], backend.clone());

        let err = ssm
            .start()
            .await
            .expect_err("start should fail on conflict");
        let err_text = err.to_string();

        assert!(err_text.contains("localhost:13000 -> cc:3000"));
        assert!(err_text.contains("already occupied by another local process"));
        assert!(backend.started_reasons().is_empty());
        assert!(backend.terminated_sessions().is_empty());
    }

    /// AWS can show multiple sessions with the same `reason` after earlier
    /// failures — all must be terminated before recreating.
    #[tokio::test]
    async fn start_recreates_tunnel_after_terminating_all_stale_aws_sessions() {
        let session = test_session(13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("stale-1", session.reason());
        backend.add_aws_session("stale-2", session.reason());
        let ssm = Ssm::with_backend(vec![session.clone()], backend.clone());

        ssm.start().await.expect("start succeeds");

        assert_eq!(
            backend.terminated_sessions(),
            vec!["stale-1".to_string(), "stale-2".to_string()]
        );
        assert_eq!(backend.started_reasons(), vec![session.reason()]);
        assert!(backend.is_local_port_listening(session.local_port));
    }

    #[tokio::test]
    async fn start_ignores_foreign_aws_sessions() {
        let session = test_session(13000, 3000);
        let foreign = test_session_with_owner(FOREIGN_OWNER_ID, 13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("foreign-1", foreign.reason());
        let ssm = Ssm::with_backend(vec![session.clone()], backend.clone());

        ssm.start().await.expect("start succeeds");

        assert!(backend.terminated_sessions().is_empty());
        assert_eq!(backend.started_reasons(), vec![session.reason()]);
        assert!(backend
            .aws_sessions
            .lock()
            .expect("aws_sessions mutex poisoned")
            .iter()
            .any(|aws_session| aws_session.session_id == "foreign-1"));
    }

    /// Unrelated SSH sessions must be left alone.
    #[tokio::test]
    async fn stop_terminates_all_matching_sessions() {
        let session_a = test_session(13000, 3000);
        let session_b = test_session(19090, 9090);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("grafana-1", session_a.reason());
        backend.add_aws_session("grafana-2", session_a.reason());
        backend.add_aws_session("prom-1", session_b.reason());
        backend.add_aws_session("ssh-1", "quake-ssh-i-1234567890".to_string());
        let ssm = Ssm::with_backend(vec![session_a, session_b], backend.clone());

        ssm.stop().await.expect("stop succeeds");

        assert_eq!(
            backend.terminated_sessions(),
            vec![
                "grafana-1".to_string(),
                "grafana-2".to_string(),
                "prom-1".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn stop_ignores_foreign_aws_sessions() {
        let session = test_session(13000, 3000);
        let foreign = test_session_with_owner(FOREIGN_OWNER_ID, 13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("local-1", session.reason());
        backend.add_aws_session("foreign-1", foreign.reason());
        let ssm = Ssm::with_backend(vec![session], backend.clone());

        ssm.stop().await.expect("stop succeeds");

        assert_eq!(backend.terminated_sessions(), vec!["local-1".to_string()]);
        assert!(backend
            .aws_sessions
            .lock()
            .expect("aws_sessions mutex poisoned")
            .iter()
            .any(|aws_session| aws_session.session_id == "foreign-1"));
    }

    #[test]
    fn list_formatted_reports_local_and_aws_state_separately() {
        let session_a = test_session(13000, 3000);
        let session_b = test_session(19090, 9090);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("grafana-1", session_a.reason());
        backend.add_aws_session("prom-1", session_b.reason());
        backend.listen_on(session_b.local_port);
        let ssm = Ssm::with_backend(vec![session_a, session_b], backend);

        let rendered = ssm.list_formatted().expect("list formatting succeeds");

        assert!(rendered.contains("localhost:13000 -> cc:3000"));
        assert!(rendered.contains("status: stale_aws"));
        assert!(rendered.contains("listener: down aws_sessions: 1"));
        assert!(rendered.contains("aws: [grafana-1:Connected]"));
        assert!(rendered.contains("localhost:19090 -> cc:9090"));
        assert!(rendered.contains("status: usable"));
        assert!(rendered.contains("listener: up aws_sessions: 1"));
        assert!(rendered.contains("aws: [prom-1:Connected]"));
    }

    #[test]
    fn list_formatted_reports_conflicting_local_listener() {
        let session = test_session(13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.listen_on(session.local_port);
        let ssm = Ssm::with_backend(vec![session], backend);

        let rendered = ssm.list_formatted().expect("list formatting succeeds");

        assert!(rendered.contains("localhost:13000 -> cc:3000"));
        assert!(rendered.contains("status: conflict"));
        assert!(rendered.contains("listener: up aws_sessions: 0"));
        assert!(rendered.contains("aws: []"));
    }

    /// Two AWS sessions with the same `reason` — both must appear in output.
    #[test]
    fn list_formatted_reports_all_matching_aws_sessions_for_one_tunnel() {
        let session = test_session(13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("grafana-1", session.reason());
        backend.add_aws_session("grafana-2", session.reason());
        let ssm = Ssm::with_backend(vec![session], backend);

        let rendered = ssm.list_formatted().expect("list formatting succeeds");

        assert!(rendered.contains("localhost:13000 -> cc:3000"));
        assert!(rendered.contains("status: stale_aws"));
        assert!(rendered.contains("listener: down aws_sessions: 2"));
        assert!(rendered.contains("aws: [grafana-1:Connected, grafana-2:Connected]"));
    }

    #[test]
    fn list_formatted_hides_foreign_aws_sessions() {
        let session = test_session(13000, 3000);
        let foreign = test_session_with_owner(FOREIGN_OWNER_ID, 13000, 3000);
        let backend = Arc::new(MockSsmBackend::default());
        backend.add_aws_session("foreign-1", foreign.reason());
        let ssm = Ssm::with_backend(vec![session], backend);

        let rendered = ssm.list_formatted().expect("list formatting succeeds");

        assert!(rendered.contains("localhost:13000 -> cc:3000"));
        assert!(rendered.contains("status: missing"));
        assert!(rendered.contains("listener: down aws_sessions: 0"));
        assert!(rendered.contains("aws: []"));
        assert!(!rendered.contains("foreign-1"));
    }
}
