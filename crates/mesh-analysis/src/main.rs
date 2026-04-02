use std::io::{self, BufRead};

use clap::Parser;
use color_eyre::eyre::{bail, Result};
use url::Url;

use arc_mesh_analysis::{
    analyze, classify_all, fetch_all_metrics, format_report, parse_all_metrics, MeshDisplayOptions,
    MeshTier,
};

#[derive(Parser)]
#[command(
    name = "arc-mesh-analysis",
    about = "Gossipsub mesh analysis for Malachite BFT networks"
)]
struct Cli {
    /// Metrics URLs (e.g. http://host:26660/metrics)
    urls: Vec<String>,

    /// Read URLs from a file (one per line, supports `name=url` format and `#` comments).
    /// Use `-` for stdin.
    #[arg(short = 'f', long = "file")]
    file: Option<String>,

    /// Show only mesh topology (skip status table)
    #[arg(long)]
    mesh_only: bool,

    /// Show per-node peer detail
    #[arg(long)]
    peers: bool,

    /// Include peer types and scores in peer detail
    #[arg(long)]
    peers_full: bool,

    /// Exit non-zero if any node is classified at this tier.
    /// Can be repeated: `--fail not-connected --fail multi-hop`.
    /// Valid tiers: fully-connected, multi-hop, not-connected.
    #[arg(long = "fail", value_name = "TIER")]
    fail_tiers: Vec<MeshTier>,
}

fn parse_urls_from_lines(lines: impl Iterator<Item = String>) -> Result<Vec<(String, Url)>> {
    let mut urls = Vec::new();
    for line in lines {
        let line = line.trim().to_string();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, raw_url)) = line.split_once('=') {
            let url = Url::parse(raw_url.trim())?;
            urls.push((name.trim().to_string(), url));
        } else {
            let url = Url::parse(&line)?;
            // Derive a name from the host
            let name = url
                .host_str()
                .map(|h| h.to_string())
                .unwrap_or_else(|| format!("node-{}", urls.len()));
            urls.push((name, url));
        }
    }
    Ok(urls)
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    let metrics_urls = if let Some(file_path) = &cli.file {
        if file_path == "-" {
            let stdin = io::stdin();
            let lines = stdin.lock().lines().map_while(Result::ok);
            parse_urls_from_lines(lines)?
        } else {
            let content = std::fs::read_to_string(file_path)?;
            parse_urls_from_lines(content.lines().map(String::from))?
        }
    } else if !cli.urls.is_empty() {
        let mut urls = Vec::new();
        for raw in &cli.urls {
            let url = Url::parse(raw)?;
            let name = url
                .host_str()
                .map(|h| h.to_string())
                .unwrap_or_else(|| format!("node-{}", urls.len()));
            urls.push((name, url));
        }
        urls
    } else {
        bail!("No metrics URLs provided. Pass URLs as arguments or use -f <file>.");
    };

    if metrics_urls.is_empty() {
        bail!("No metrics URLs provided.");
    }

    let raw_metrics = fetch_all_metrics(&metrics_urls).await;
    let nodes_data = parse_all_metrics(&raw_metrics);
    let analysis = analyze(&nodes_data);

    let options = MeshDisplayOptions {
        show_counts: !cli.mesh_only,
        show_mesh: true,
        show_peers: cli.peers || cli.peers_full,
        show_peers_full: cli.peers_full,
    };

    print!("{}", format_report(&analysis, &options));

    if !cli.fail_tiers.is_empty() {
        let classifications = classify_all(&analysis);

        println!("\n--- Tier Classification ---");
        for (moniker, node_type, tier) in &classifications {
            println!("  {moniker} ({node_type}): {tier}");
        }

        let failing: Vec<_> = classifications
            .iter()
            .filter(|(_, _, tier)| cli.fail_tiers.contains(tier))
            .collect();

        if !failing.is_empty() {
            println!();
            for (moniker, node_type, tier) in &failing {
                println!("FAIL: {moniker} ({node_type}) is {tier}");
            }
            bail!("{} node(s) in failing tier(s)", failing.len());
        }

        println!("\nAll nodes pass tier checks.");
    }

    Ok(())
}
