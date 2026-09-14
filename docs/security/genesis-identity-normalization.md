# Genesis identity normalization

## Finding

`schemaAddress` and `schemaHex` accept mixed-case hexadecimal identities, while several genesis validation checks historically compared the textual representation directly. Hexadecimal casing does not change the represented bytes, so two spellings of the same address or fixed-size public key can bypass string-based uniqueness or role-separation checks.

## Affected validation paths

- Validator public-key uniqueness
- Validator-registerer uniqueness
- Controller uniqueness across validators
- Operator/role versus proxy-admin separation
- NativeFiatToken minter uniqueness

The ValidatorRegistry proxy-address comparison is not an affected casing path: the current hard-coded system-contract address contains only numeric hexadecimal digits, so no alternate mixed-case spelling can exist.

## Impact

This is a genesis/configuration integrity issue rather than a standalone permissionless runtime exploit. A malformed configuration could represent the same underlying identity more than once where validation intends uniqueness or role separation.

For NativeFiatToken minters, address-keyed storage is derived from the decoded address value. Case variants therefore resolve to the same mapping slots; conflicting allowance entries can overwrite one another during genesis allocation construction instead of being rejected as duplicate minters.

## Fix

Normalize hexadecimal identities only at comparison boundaries with `toLowerCase()`. Keep the original input unchanged for serialization and diagnostics. The shared proxy-admin helper normalizes both operands, and NativeFiatToken minter uniqueness normalizes the address used as the `Set` key.

## Regression coverage

Regression tests cover mixed-case collisions for:

- Validator public keys
- Validator-registerer addresses
- Controllers
- Operator/proxy-admin role separation
- NativeFiatToken minters

The tests also retain positive cases for valid distinct/compatible configurations.

## Validation

Focused tests:

```bash
npx hardhat test ./tests/unit/validator-manager-genesis-validation.test.ts ./tests/unit/native-fiat-token-genesis-validation.test.ts --no-compile
```

Repository validation:

```bash
make test-unit-hardhat
make lint
```

The repository workflow does not currently invoke `make test-unit-hardhat`, and its ESLint command excludes `scripts/` and `tests/`, so local execution of the focused suite remains an explicit validation requirement.

## Scope

EIP-55 checksum enforcement is intentionally not part of this change. It would be a broader input-validation policy change and should be reviewed independently.
