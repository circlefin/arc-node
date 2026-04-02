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

//! Types for RPC synchronization endpoint configuration.

use core::fmt;
use std::str::FromStr;

use url::Url;

/// A parsed endpoint URL for RPC synchronization.
///
/// This type holds an HTTP URL and an optional WebSocket URL/port for connecting
/// to a sync endpoint. If no WebSocket URL/port is explicitly provided, one is
/// derived from the HTTP URL by converting the scheme (`http` -> `ws`, `https` -> `wss`).
///
/// # String Format
///
/// The string representation uses the format:
/// ```text
/// <http_url>[,<ws_scheme>=<ws_url|ws_port>]
/// ```
///
/// Examples:
/// - `http://localhost:8545` - HTTP only, WebSocket derived as `ws://localhost:8546` (HTTP port + 1)
/// - `https://example.com:8545,wss=8546` - HTTPS with explicit WSS on port 8546
/// - `https://example.com,wss=ws.example.com` - HTTPS with WSS on a different host
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEndpointUrl {
    http: Url,
    ws: Option<Url>,
}

impl SyncEndpointUrl {
    /// Returns a reference to the HTTP URL.
    pub fn http(&self) -> &Url {
        &self.http
    }

    /// Returns the WebSocket URL.
    ///
    /// If an explicit WebSocket URL was provided during parsing, it is returned.
    /// Otherwise, the WebSocket URL is derived from the HTTP URL by converting
    /// the scheme (`http` -> `ws`, `https` -> `wss`). If the HTTP URL uses a
    /// non-default port, the WebSocket port is set to HTTP port + 1.
    pub fn websocket(&self) -> Url {
        self.ws.clone().unwrap_or_else(|| {
            let mut ws_url = self.http.clone();
            let ws_scheme = http_to_ws_scheme(self.http.scheme());
            ws_url
                .set_scheme(ws_scheme)
                .expect("valid WebSocket scheme");

            // If HTTP uses a non-default port, set WS port to HTTP port + 1
            if let Some(http_port) = self.http.port() {
                ws_url.set_port(Some(http_port + 1)).expect("valid port");
            }

            ws_url
        })
    }
}

/// Converts an HTTP scheme to its WebSocket equivalent.
fn http_to_ws_scheme(scheme: &str) -> &'static str {
    match scheme {
        "http" => "ws",
        "https" => "wss",
        _ => unreachable!("validated scheme"),
    }
}

/// Validates that the given scheme is a valid HTTP scheme.
fn validate_http_scheme(scheme: &str) -> Result<(), eyre::Report> {
    if scheme != "http" && scheme != "https" {
        return Err(eyre::eyre!(
            "Invalid HTTP URL scheme '{scheme}'. Must be 'http' or 'https'."
        ));
    }
    Ok(())
}

/// Validates that the given scheme is a valid WebSocket scheme.
fn validate_ws_scheme(scheme: &str) -> Result<(), eyre::Report> {
    if scheme != "ws" && scheme != "wss" {
        return Err(eyre::eyre!(
            "Invalid WebSocket protocol '{scheme}'. Must be 'ws' or 'wss'."
        ));
    }
    Ok(())
}

/// Parses a WebSocket override in the format `<scheme>=<value>`.
///
/// The value after `=` can be:
/// - A port number (e.g., `wss=8546`) — uses the base URL's host with the given port
/// - A hostname (e.g., `wss=ws.example.com`) — uses the given host with the scheme's default port
/// - A host:port pair (e.g., `wss=ws.example.com:1212`) — uses both the given host and port
/// - A hostname with path (e.g., `wss=ws.example.com/websocket`) — preserves the path
fn parse_ws_override(s: &str, base_url: &Url) -> Result<Url, eyre::Report> {
    let (scheme, value) = s
        .split_once('=')
        .ok_or_else(|| eyre::eyre!("Invalid WebSocket part format: {s}"))?;

    validate_ws_scheme(scheme)?;

    if let Ok(port) = value.parse::<u16>() {
        let mut ws_url = base_url.clone();
        ws_url.set_scheme(scheme).expect("valid scheme");
        ws_url.set_port(Some(port)).expect("valid port");
        return Ok(ws_url);
    }

    let ws_url = Url::parse(&format!("{scheme}://{value}"))
        .map_err(|e| eyre::eyre!("Failed to parse WebSocket URL: {e}"))?;

    Ok(ws_url)
}

impl FromStr for SyncEndpointUrl {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (http_part, ws_part) = s.split_once(',').map_or((s, None), |(h, w)| (h, Some(w)));

        let http =
            Url::parse(http_part).map_err(|e| eyre::eyre!("Failed to parse HTTP URL: {e}"))?;

        validate_http_scheme(http.scheme())?;

        let ws = ws_part
            .map(|part| parse_ws_override(part, &http))
            .transpose()?;

        Ok(Self { http, ws })
    }
}

impl fmt::Display for SyncEndpointUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = self.http.host_str().expect("validated host");
        let http_port = self.http.port_or_known_default().expect("validated port");
        let ws_url = self.websocket();
        let ws_host = ws_url.host_str().expect("validated host");

        write!(
            f,
            "{}://{host}:{http_port},{}=",
            self.http.scheme(),
            ws_url.scheme()
        )?;

        let ws_path = ws_url.path();
        let has_path = ws_path != "/";

        if ws_host != host || has_path {
            // Include the host when it differs or when a path is present
            // (a bare port + path like `443/websocket` mis-parses as a hostname)
            write!(f, "{ws_host}")?;
            if let Some(ws_port) = ws_url.port() {
                write!(f, ":{ws_port}")?;
            }
        } else {
            // Same host, no path — just the port
            let ws_port = ws_url.port_or_known_default().expect("validated port");
            write!(f, "{ws_port}")?;
        }

        if has_path {
            write!(f, "{ws_path}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_only() {
        let url: SyncEndpointUrl = "http://localhost:8545".parse().unwrap();
        assert_eq!(url.http().as_str(), "http://localhost:8545/");
        // WS port is HTTP port + 1 when no override specified
        assert_eq!(url.websocket().as_str(), "ws://localhost:8546/");
    }

    #[test]
    fn parse_https_only() {
        let url: SyncEndpointUrl = "https://example.com:8545".parse().unwrap();
        assert_eq!(url.http().as_str(), "https://example.com:8545/");
        // WSS port is HTTPS port + 1 when no override specified
        assert_eq!(url.websocket().as_str(), "wss://example.com:8546/");
    }

    #[test]
    fn parse_http_default_port() {
        let url: SyncEndpointUrl = "http://localhost".parse().unwrap();
        assert_eq!(url.http().as_str(), "http://localhost/");
        // Default HTTP port (80) uses default WS port (80)
        assert_eq!(url.websocket().as_str(), "ws://localhost/");
    }

    #[test]
    fn parse_https_default_port() {
        let url: SyncEndpointUrl = "https://example.com".parse().unwrap();
        assert_eq!(url.http().as_str(), "https://example.com/");
        // Default HTTPS port (443) uses default WSS port (443)
        assert_eq!(url.websocket().as_str(), "wss://example.com/");
    }

    #[test]
    fn parse_http_with_ws_override() {
        let url: SyncEndpointUrl = "http://localhost:8545,ws=8546".parse().unwrap();
        assert_eq!(url.http().as_str(), "http://localhost:8545/");
        assert_eq!(url.websocket().as_str(), "ws://localhost:8546/");
    }

    #[test]
    fn parse_https_with_wss_override() {
        let url: SyncEndpointUrl = "https://example.com:8545,wss=8546".parse().unwrap();
        assert_eq!(url.http().as_str(), "https://example.com:8545/");
        assert_eq!(url.websocket().as_str(), "wss://example.com:8546/");
    }

    #[test]
    fn parse_http_with_wss_override() {
        let url: SyncEndpointUrl = "http://localhost:8545,wss=8546".parse().unwrap();
        assert_eq!(url.http().as_str(), "http://localhost:8545/");
        assert_eq!(url.websocket().as_str(), "wss://localhost:8546/");
    }

    #[test]
    fn error_invalid_http_scheme() {
        let err = "ftp://localhost:8545"
            .parse::<SyncEndpointUrl>()
            .unwrap_err();
        assert!(err.to_string().contains("Invalid HTTP URL scheme"));
    }

    #[test]
    fn error_invalid_ws_scheme() {
        let err = "http://localhost:8545,tcp=8546"
            .parse::<SyncEndpointUrl>()
            .unwrap_err();
        assert!(err.to_string().contains("Invalid WebSocket protocol"));
    }

    #[test]
    fn error_invalid_ws_format() {
        let err = "http://localhost:8545,ws8546"
            .parse::<SyncEndpointUrl>()
            .unwrap_err();
        assert!(err.to_string().contains("Invalid WebSocket part format"));
    }

    #[test]
    fn parse_ws_host_override() {
        let url: SyncEndpointUrl = "http://localhost:8545,ws=notaport".parse().unwrap();
        assert_eq!(url.http().as_str(), "http://localhost:8545/");
        assert_eq!(url.websocket().as_str(), "ws://notaport/");
    }

    #[test]
    fn parse_https_with_wss_host_override() {
        let url: SyncEndpointUrl = "https://rpc.example.com,wss=rpc-ws.example.com"
            .parse()
            .unwrap();
        assert_eq!(url.http().as_str(), "https://rpc.example.com/");
        assert_eq!(url.websocket().as_str(), "wss://rpc-ws.example.com/");
    }

    #[test]
    fn parse_wss_with_path_override() {
        let url: SyncEndpointUrl =
            "https://rpc.testnet.example.com,wss=rpc.testnet.example.com/websocket"
                .parse()
                .unwrap();
        assert_eq!(url.http().as_str(), "https://rpc.testnet.example.com/");
        assert_eq!(
            url.websocket().as_str(),
            "wss://rpc.testnet.example.com/websocket"
        );

        let reparsed: SyncEndpointUrl = url.to_string().parse().unwrap();
        assert_eq!(url, reparsed, "roundtrip failed for path override");
    }

    #[test]
    fn parse_https_port_with_wss_host_override() {
        let url: SyncEndpointUrl = "https://example.com:8545,wss=ws.example.com"
            .parse()
            .unwrap();
        assert_eq!(url.http().as_str(), "https://example.com:8545/");
        assert_eq!(url.websocket().as_str(), "wss://ws.example.com/");
    }

    #[test]
    fn display_with_ws_host_override() {
        let url: SyncEndpointUrl = "https://rpc.example.com,wss=rpc-ws.example.com"
            .parse()
            .unwrap();
        assert_eq!(
            url.to_string(),
            "https://rpc.example.com:443,wss=rpc-ws.example.com"
        );
    }

    #[test]
    fn display_http_only() {
        let url: SyncEndpointUrl = "http://localhost:8545".parse().unwrap();
        assert_eq!(url.to_string(), "http://localhost:8545,ws=8546");
    }

    #[test]
    fn display_https_only() {
        let url: SyncEndpointUrl = "https://example.com:8545".parse().unwrap();
        assert_eq!(url.to_string(), "https://example.com:8545,wss=8546");
    }

    #[test]
    fn display_with_ws_override() {
        let url: SyncEndpointUrl = "http://localhost:8545,ws=8546".parse().unwrap();
        assert_eq!(url.to_string(), "http://localhost:8545,ws=8546");
    }

    #[test]
    fn wss_with_host_port_override() {
        let url: SyncEndpointUrl = "https://example.com:443,wss=ws.example.com:1212"
            .parse()
            .unwrap();
        assert_eq!(url.http().as_str(), "https://example.com/");
        assert_eq!(url.websocket().as_str(), "wss://ws.example.com:1212/");
    }

    #[test]
    fn parse_wss_with_host_port_and_path_override() {
        let url: SyncEndpointUrl = "https://example.com,wss=ws.example.com:8546/websocket"
            .parse()
            .unwrap();
        assert_eq!(url.http().as_str(), "https://example.com/");
        assert_eq!(
            url.websocket().as_str(),
            "wss://ws.example.com:8546/websocket"
        );
    }

    #[test]
    fn roundtrip() {
        let inputs = [
            "http://localhost:8545,ws=8546",
            "https://example.com:8545,wss=8546",
            "http://127.0.0.1:9000,ws=9001",
            "https://node.example.org:9000,wss=8546",
            "https://rpc.testnet.example.com,wss=rpc.testnet.example.com/websocket",
            "https://example.com,wss=ws.example.com:8546/websocket",
        ];

        for input in inputs {
            let url: SyncEndpointUrl = input.parse().unwrap();
            let output = url.to_string();
            let reparsed: SyncEndpointUrl = output.parse().unwrap();
            assert_eq!(url, reparsed, "roundtrip failed for {input}");
        }
    }
}
