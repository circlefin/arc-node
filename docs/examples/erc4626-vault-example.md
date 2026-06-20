# ERC-4626 Tokenized Vault Example on Arc Testnet

This example shows how to build an ERC-4626 tokenized vault for native USDC on Arc Testnet, including an agent-yield deposit path for Multi-Agent Orchestrator integration.

## Overview

ERC-4626 is the standard interface for tokenized yield-bearing vaults. ArcUSDCVault wraps Arc Testnet's native USDC, issuing avUSDC shares that represent proportional ownership of the vault's underlying assets. The owner can inject yield directly, increasing the value of all outstanding shares without minting new ones.

## Contract on Arc Testnet

| Field | Value |
|-------|-------|
| Contract | 0x6C13dA317B65474299F6fDee02daDd6626Eb2BFe |
| Underlying asset | Arc Testnet USDC (0x3600000000000000000000000000000000000000) |
| Share token | Arc USDC Vault Share (avUSDC) |

## Implementation

Built with OpenZeppelin v5.6.1's ERC4626 base contract:

```solidity
contract ArcUSDCVault is ERC4626, Ownable {
    using SafeERC20 for IERC20;

    constructor(IERC20 asset_)
        ERC20("Arc USDC Vault Share", "avUSDC")
        ERC4626(asset_)
        Ownable(msg.sender)
    {}

    function depositYield(uint256 amount) external onlyOwner {
        IERC20(asset()).safeTransferFrom(msg.sender, address(this), amount);
        emit YieldDeposited(msg.sender, amount, totalAssets());
    }

    function depositForAgent(address agent, uint256 missionId, uint256 amount)
        external
        returns (uint256 shares)
    {
        shares = deposit(amount, agent);
        emit AgentYieldDeposited(agent, missionId, amount, shares);
    }
}
```

## Multi-Agent Orchestrator Integration

The depositForAgent function lets an orchestrator route mission payouts directly into yield-bearing shares instead of raw USDC, tagging each deposit with a mission ID for on-chain traceability. This composes naturally with the MultiAgentOrchestrator contract from a previous example in this series: instead of an agent receiving a raw USDC payout, the orchestrator can call depositForAgent on the agent's behalf, immediately putting their earnings into a yield-bearing position.

## Test Coverage

13 tests covering deposit, withdraw, redeem, multiple depositors with proportional shares, yield injection increasing share value, yield benefiting only existing holders, access control, agent-tagged deposits, and a full deposit-yield-redeem cycle.

```bash
forge test

# Ran 13 tests for test/ArcUSDCVault.t.sol:ArcUSDCVaultTest
# [PASS] testDeposit()
# [PASS] testDepositForAgent()
# [PASS] testDepositForAgentEmitsEvent()
# [PASS] testDepositForAgentRevertsOnZeroAddress()
# [PASS] testDepositYieldRevertsForNonOwner()
# [PASS] testDepositYieldRevertsOnZero()
# [PASS] testFullDepositWithdrawCycleWithYield()
# [PASS] testInitialState()
# [PASS] testMultipleDepositorsProportionalShares()
# [PASS] testRedeem()
# [PASS] testWithdraw()
# [PASS] testYieldBenefitsExistingHoldersOnly()
# [PASS] testYieldIncreasesShareValue()
# Suite result: ok. 13 passed; 0 failed
```

## Live Verified Example

A real deposit was made and verified on Arc Testnet:

- Approve tx: https://testnet.arcscan.app/tx/0x5889cee125030d6fb092926689b14c684a6dbe0a01b51edb354c19b64e7e40ee
- Deposit tx: https://testnet.arcscan.app/tx/0x3db09d1f8cdc5f1506a48420c92bb6ee4fa6c90c31a6ab4441b41c92c2dc9c5d
- Result: 5 USDC deposited, 5,000,000 avUSDC shares minted (1:1 first-deposit ratio)

## GitHub Repository

Full implementation: https://github.com/consumeobeydie/arc-vault

## Resources

- ERC-4626 EIP: https://eips.ethereum.org/EIPS/eip-4626
- OpenZeppelin ERC4626: https://docs.openzeppelin.com/contracts/5.x/api/token/erc20#ERC4626
- Arc Testnet Explorer: https://testnet.arcscan.app
- Arc Contract Addresses: https://docs.arc.io/arc/references/contract-addresses
