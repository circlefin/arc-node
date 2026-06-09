# Arc MCP Server

A Model Context Protocol (MCP) server that gives AI tools like Claude Code direct access to Arc Testnet blockchain data.

## What is MCP?

Model Context Protocol (MCP) is an open standard that lets AI tools discover and use external tools and data sources. With this Arc MCP server, Claude Code can query Arc Testnet directly during conversations.

## Available Tools

| Tool | Description |
|------|-------------|
| get_balance | Get USDC balance for any address |
| get_transaction | Get transaction details by hash |
| get_block | Get latest block information |
| get_tx_count | Get transaction count for an address |
| get_agent_info | Get ERC-8004 agent information |
| get_job_status | Get ERC-8183 job status |
| get_network_info | Get network info and contract addresses |

## Installation

git clone https://github.com/consumeobeydie/arc-mcp-server.git
cd arc-mcp-server
npm install

## Add to Claude Code

claude mcp add --transport stdio arc-testnet node /path/to/arc-mcp-server/src/index.js

## Example Usage

Once added, ask Claude Code:
- "Get the USDC balance of 0x54b4B44749a95070560509B6Ec0be501665CcF63"
- "Get Arc Testnet network info"
- "Get ERC-8004 agent info"
- "Get status of job 110935"

## Example Output

Claude Code returns structured data like:

Network: Arc Testnet
Chain ID: 5042002
Gas Token: USDC
Finality: Sub-second deterministic
Latest Block: 46,336,076

Contracts:
- USDC: 0x3600000000000000000000000000000000000000
- IdentityRegistry: 0x8004A818BFB912233c491871b3d84c89A494BD9e
- AgenticCommerce: 0x0747EEf0706327138c69792bF28Cd525089e4583

## GitHub Repository

https://github.com/consumeobeydie/arc-mcp-server

## Resources

- Arc MCP Docs: https://docs.arc.io/ai/mcp
- Model Context Protocol: https://modelcontextprotocol.io
- Arc Testnet Explorer: https://testnet.arcscan.app
