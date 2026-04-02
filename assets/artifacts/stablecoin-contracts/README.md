# Stablecoin Contract Artifacts

This directory contains pre-compiled Hardhat JSON artifacts for the USDC ERC-20 implementation on Arc.

## Contracts

- **SignatureChecker.json** - Library for signature verification (linked to NativeFiatTokenV2_2)
- **NativeFiatTokenV2_2.json** - USDC ERC-20 implementation contract with Arc-specific modifications
- **FiatTokenProxy.json** - USDC transparent proxy contract

## Deployment

The genesis builder (`contracts/scripts/ArtifactHelper.s.sol`) uses these artifacts to set the USDC contract bytecode in the Arc genesis block for local development.
