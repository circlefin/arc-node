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

use color_eyre::eyre::{eyre, Context, Ok, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::infra::{remote, NodeInfraData};
use crate::shell;
use crate::util::in_parallel;

/// Manage SSM sessions to forward local ports to remote ports in the Control
/// Center server.
///
/// SSM sessions are port-forwarding tunnels using StartPortForwardingSession:
/// only the handshake phase is throttled; once established, tunnels remain
/// active in the background.
///
/// Note: SSH/SCP commands to nodes are routed through CC (Control Center) using
/// nodes' private IPs, avoiding additional SSM sessions. Only direct SSH to CC
/// uses StartSSHSession.
#[derive(Clone)]
pub(crate) struct Ssm(Vec<SSMSession>);

impl Ssm {
    /// Initialize SSM sessions to the Control Center server, if present.
    pub fn new(cc: Option<&NodeInfraData>) -> Result<Self> {
        let mut ssm_sessions = Vec::new();

        // Create (not started yet) SSM sessions to CC
        if let Some(cc) = cc {
            if let Some(ssm_tunnel_ports) = cc.ssm_tunnel_ports.as_ref() {
                let instance_id = cc.instance_id().wrap_err("Instance ID not found for CC")?;
                for (remote_port, local_port) in ssm_tunnel_ports.iter() {
                    ssm_sessions.push(SSMSession::new(
                        remote::CC_INSTANCE.to_string(),
                        instance_id.clone(),
                        *local_port,
                        *remote_port,
                    ));
                }
            }
        }

        Ok(Self(ssm_sessions))
    }

    /// Start all inactive SSM sessions in parallel
    pub async fn start(&self) -> Result<()> {
        debug!("Starting SSM sessions");

        let (_, inactive_sessions) = self.sessions_partitioned_active()?;
        let reasons: Vec<_> = inactive_sessions.iter().map(|s| s.reason()).collect();

        let start_results =
            in_parallel(&inactive_sessions, |s| async move { s.start().await }).await;
        for (session, result) in inactive_sessions.iter().zip(start_results) {
            if let Err(e) = result {
                return Err(eyre!(
                    "Failed to start SSM tunnel {}: {e}",
                    session.reason()
                ));
            }
        }

        // Wait for the sessions to be active. The threads running `start-session`
        // will terminate when their parent thread stops. Once all connections are
        // established, the SSM tunnels stay active in the background.
        debug!("Waiting for started SSM sessions to be ready",);
        self.wait_for_connections(&reasons, Duration::from_secs(60))
            .await?;

        info!("✅ SSM sessions started");
        Ok(())
    }

    /// Stop all active SSM sessions in parallel
    pub async fn stop(&self) -> Result<()> {
        debug!("Closing SSM sessions");

        let (active_sessions, _) = self.sessions_partitioned_active()?;
        let stop_results = in_parallel(&active_sessions, |s| async move { s.stop().await }).await;
        for (session, result) in active_sessions.iter().zip(stop_results) {
            if let Err(e) = result {
                return Err(eyre!("Failed to stop SSM tunnel {}: {e}", session.reason()));
            }
        }

        info!("✅ SSM sessions terminated");
        Ok(())
    }

    /// List all active SSM sessions
    pub async fn list(&self) -> Result<()> {
        println!("{}", self.list_formatted()?);
        Ok(())
    }

    /// Return a formatted string of all active SSM sessions.
    pub fn list_formatted(&self) -> Result<String> {
        let active_sessions_map = self.active_sessions_map()?;

        let mut result = active_sessions_map
            .iter()
            .map(|(reason, (session_id, start_date, status))| {
                format!("  - {session_id:>42} {start_date:>24} {status:>10} {reason}")
            })
            .collect::<Vec<_>>()
            .join("\n");

        if result.is_empty() {
            result.push_str("  - No active SSM sessions");
        }
        result.push('\n');

        let num_disconnected = self.0.len().saturating_sub(active_sessions_map.len());
        if num_disconnected > 0 {
            result.push_str(&format!("  - {num_disconnected} SSM tunnels are disconnected: run `setup` or `remote ssm start` to restart the sessions (times out after 20 minutes of inactivity)."));
        }

        Ok(format!(
            "Active SSM tunnels (session ID, start date, status, reason):\n{result}"
        ))
    }

    /// Get a map of active sessions (Reason -> (SessionId, StartDate, Status))
    fn active_sessions_map(&self) -> Result<HashMap<String, (String, String, String)>> {
        let filter = self
            .0
            .iter()
            .map(|session| format!("Target==`{}`", session.instance_id))
            .collect::<Vec<_>>()
            .join(" || ");
        let query = format!("Sessions[?{filter}]  | sort_by([], &to_string(Reason))[*] | [].[SessionId, StartDate, Status, Reason]");
        #[rustfmt::skip]
        let args = vec![
            "ssm", "describe-sessions",
            "--state", "Active",
            "--output", "text",
            "--query", &query,
        ];

        let result = shell::exec_with_output("aws", args, Path::new("."))
            .wrap_err("Failed to query active SSM sessions")?;

        // Parse the output into a map of session reasons to session data
        let mut map = HashMap::new();
        for line in result.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Expected output: SessionId StartDate Status Reason
            if parts.len() >= 4 {
                let session_id = parts[0].to_string();
                let start_date = parts[1].to_string();
                let status = parts[2].to_string();
                let reason = parts[3].to_string();
                map.insert(reason, (session_id, start_date, status));
            }
        }
        Ok(map)
    }

    /// Partition the sessions into active and inactive.
    fn sessions_partitioned_active(&self) -> Result<(Vec<&SSMSession>, Vec<&SSMSession>)> {
        let active_sessions_map = self.active_sessions_map()?;
        Ok(self
            .0
            .iter()
            .partition(|s| active_sessions_map.contains_key(&s.reason())))
    }

    /// Wait for all given sessions to be ready to use
    async fn wait_for_connections(
        &self,
        sessions_reasons: &[String],
        timeout: Duration,
    ) -> Result<()> {
        let start_time = Instant::now();
        while start_time.elapsed() < timeout {
            if self.all_sessions_active(sessions_reasons)? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(eyre!("Timeout waiting for SSM tunnels to be active"))
    }

    /// Check if all given sessions are active
    fn all_sessions_active(&self, sessions_reasons: &[String]) -> Result<bool> {
        let active_sessions_map = self.active_sessions_map()?;
        let active_session_reasons = active_sessions_map.keys().cloned().collect::<Vec<_>>();
        for reason in sessions_reasons.iter() {
            if !active_session_reasons.contains(reason) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// A single SSM tunnel from a local port to a remote port in an EC2 instance.
#[derive(Clone)]
pub(crate) struct SSMSession {
    instance_name: String,
    instance_id: String,
    local_port: usize,
    remote_port: usize,
}

impl SSMSession {
    pub fn new(
        instance_name: String,
        instance_id: String,
        local_port: usize,
        remote_port: usize,
    ) -> Self {
        Self {
            instance_name,
            instance_id,
            local_port,
            remote_port,
        }
    }

    /// String that uniquely identifies the session.
    ///
    /// We use the `reason` field in the SSM commands to uniquely identify
    /// sessions. The start-session command run in interactive mode, so we can't
    /// easily retrieve the session ID. This is needed to terminate the session.
    ///
    /// Instance ID and local port are enough to uniquely identify the session,
    /// but we include other data for debugging.
    fn reason(&self) -> String {
        format!(
            "quake-{}-{}-{}-{}",
            self.instance_name, self.instance_id, self.local_port, self.remote_port
        )
    }

    /// Start SSM session in the background
    pub async fn start(&self) -> Result<()> {
        debug!(instance_id=%self.instance_id, local_port=%self.local_port, "Starting SSM tunnel");

        // Spawn task to run the SSM tunnel in the background
        let reason = self.reason();
        let instance_id = self.instance_id.to_owned();
        let local_port = self.local_port;
        let remote_port = self.remote_port;
        tokio::spawn(async move {
            #[rustfmt::skip]
            let args = [
                "ssm", "start-session",
                "--target", &instance_id,
                "--reason", &reason,
                "--document-name", "AWS-StartPortForwardingSession",
                "--parameters", &format!("{{\"portNumber\":[\"{remote_port}\"],\"localPortNumber\":[\"{local_port}\"]}}"),
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

    /// Stop the SSM tunnel
    pub async fn stop(&self) -> Result<()> {
        debug!(instance_id=%self.instance_id, local_port=%self.local_port, "Stopping SSM tunnel");

        let Some(session_id) = self.get_session_id()? else {
            warn!(%self.instance_id, "No active SSM session found");
            return Ok(());
        };

        // Terminate session
        let args = ["ssm", "terminate-session", "--session-id", &session_id];
        shell::exec("aws", args.to_vec(), Path::new("."), None, false)
    }

    /// Retrieve the session ID of the active SSM tunnel
    fn get_session_id(&self) -> Result<Option<String>> {
        let query = format!("Sessions[?Reason==`{}`].SessionId", self.reason());

        #[rustfmt::skip]
        let args = vec![
            "ssm", "describe-sessions",
            "--state", "Active",
            "--output", "text",
            "--query", &query,
        ];
        let session_id = shell::exec_with_output("aws", args, Path::new("."))?;
        Ok((!session_id.is_empty()).then_some(session_id))
    }
}
