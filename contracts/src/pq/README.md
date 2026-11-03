# Post quantum precompile usage

The precompile at `Precompiles.PQ` verifies an SLH-DSA-SHA2-128s signature over the exact message bytes you pass. It injects nothing of its own. It does not add a chain id, a verifying contract or a purpose, the same way `ecrecover` returns a signer without binding either. A raw signature is therefore valid on any chain, in any contract and for any action, unless the caller binds those into the message that gets signed.

Bind the domain into the signed message. Build an EIP 712 digest over the chain id, the verifying contract and a purpose, then pass that 32 byte digest as the message. Altering any domain field changes the digest, so a signature made for one chain, contract or purpose stops verifying everywhere else.

A domain stops cross domain reuse. It does not stop replay inside the same domain. The precompile verifies the same valid signature every time it is called. For any state changing authorization, enforce an application nonce and a deadline, and record the nonce as used before you verify.

`examples/PQAuthExample.sol` shows the full pattern. The checklist I follow.

1. Build an EIP 712 digest over chain id, verifying contract and a purpose.
2. Pass that digest as the message, never a raw application string.
3. Mark the nonce used before the verify call so a later revert cannot free it.
4. Check the deadline.
5. Read a true result as proof of the signature only. Authorization stays your contract's job.
