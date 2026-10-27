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

//! Inline bytecode for minimal test contracts.

use alloy_primitives::{keccak256, Address, Bytes};
use revm_bytecode::opcode::*;

/// Contract with `receive() external payable {}` — accepts value, does nothing.
///
/// ```eas
/// stop  ;; [] — halt execution, accept any value sent
/// ```
pub fn payable_contract_deploy_code() -> Bytes {
    let runtime = [STOP];
    deploy_code(&runtime)
}

/// Contract that always reverts with zero-length revert data.
///
/// ```eas
/// push1 0x00  ;; [0]    — revert data length
/// push1 0x00  ;; [0, 0] — revert data offset
/// revert      ;; []      — revert(0, 0)
/// ```
pub fn reverting_contract_deploy_code() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH1, 0x00, // revert(0,
        PUSH1, 0x00, //   0)
        REVERT,
    ];
    deploy_code(&runtime)
}

/// Constructor that reverts during deployment (no contract is created).
/// This is NOT wrapped in deploy_code — it IS the initcode that reverts.
///
/// ```eas
/// ;; Initcode — reverts immediately, so CREATE/CREATE2 returns address(0).
/// push1 0x00  ;; [0]    — revert data length
/// push1 0x00  ;; [0, 0] — revert data offset
/// revert      ;; []      — revert(0, 0)
/// ```
pub fn reverting_constructor_code() -> Bytes {
    #[rustfmt::skip]
    let initcode = vec![
        PUSH1, 0x00, // revert(0,
        PUSH1, 0x00, //   0)
        REVERT,
    ];
    Bytes::from(initcode)
}

/// Contract that calls SELFDESTRUCT with beneficiary from calldata.
///
/// ```eas
/// push1 0x00     ;; [0]           — calldata offset
/// calldataload   ;; [cd[0..32]]   — load 32-byte word
/// push1 0x60     ;; [96, cd]      — shift amount (256 - 160)
/// shr            ;; [addr]        — isolate 20-byte address
/// selfdestruct   ;; []            — send balance to addr and destroy
/// ```
pub fn selfdestruct_contract_deploy_code() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH1, 0x00, CALLDATALOAD, // calldataload(0) → 32-byte word
        PUSH1, 0x60, SHR,          // shr(96) → target address
        SELFDESTRUCT,               // selfdestruct(addr)
    ];
    deploy_code(&runtime)
}

/// Contract that warms a calldata target with BALANCE, then calls SELFDESTRUCT
/// with the same target.
///
/// ```eas
/// push1 0x00     ;; [0]           - calldata offset
/// calldataload   ;; [cd[0..32]]   - load 32-byte word
/// push1 0x60     ;; [96, cd]      - shift amount (256 - 160)
/// shr            ;; [addr]        - isolate 20-byte address
/// dup1           ;; [addr, addr]  - keep addr for SELFDESTRUCT
/// balance        ;; [addr, bal]   - warm addr by reading its balance
/// pop            ;; [addr]        - discard balance
/// selfdestruct   ;; []            - send balance to warmed addr and destroy
/// ```
pub fn balance_warming_selfdestruct_contract_deploy_code() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH1, 0x00, CALLDATALOAD, // [cd[0..32]]
        PUSH1, 0x60, SHR,          // [addr]
        DUP1,                      // [addr, addr]
        BALANCE,                   // [addr, balance]
        POP,                       // [addr]
        SELFDESTRUCT,              // []
    ];
    deploy_code(&runtime)
}

/// Contract that forwards received value to a target address via CALL.
///
/// Pseudocode: `call(gas(), calldata[0:20], callvalue(), 0, 0, 0, 0)`
///
/// ```eas
/// ;; Extract target address from calldata[0:20].
/// push1 0x00     ;; [0]                                      — calldata offset
/// calldataload   ;; [cd[0..32]]                              — load 32-byte word
/// push1 0x60     ;; [96, cd[0..32]]                          — shift amount
/// shr            ;; [addr]                                   — isolate 20-byte address
///
/// ;; Build CALL args (reverse order — EVM is stack-based).
/// push1 0x00     ;; [0, addr]                                — retLength
/// push1 0x00     ;; [0, 0, addr]                             — retOffset
/// push1 0x00     ;; [0, 0, 0, addr]                          — argsLength
/// push1 0x00     ;; [0, 0, 0, 0, addr]                       — argsOffset
/// callvalue      ;; [value, 0, 0, 0, 0, addr]                — msg.value
/// dup6           ;; [addr, value, 0, 0, 0, 0, addr]          — copy target
/// gas            ;; [gas, addr, value, 0, 0, 0, 0, addr]     — remaining gas
/// call           ;; [success, addr]                           — call(gas, addr, value, 0, 0, 0, 0)
/// stop           ;; []                                        — halt
/// ```
pub fn forwarder_contract_deploy_code() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH1, 0x00, CALLDATALOAD,  // calldataload(0) → 32-byte word
        PUSH1, 0x60, SHR,           // shr(96) → target address

        PUSH1, 0x00,                // call(gas(),
        PUSH1, 0x00,                //   addr,
        PUSH1, 0x00,                //   callvalue(),
        PUSH1, 0x00,                //   0, 0, 0, 0)
        CALLVALUE,                   //
        DUP6,                        // ↑ addr from stack position 6
        GAS,                         //
        CALL,                        // → success
        STOP,
    ];
    deploy_code(&runtime)
}

/// Like [`forwarder_contract_deploy_code`] but always succeeds — inner CALL
/// result is discarded with POP so the outer frame never reverts.
///
/// Pseudocode: `pop(call(gas(), calldata[0:20], callvalue(), 0, 0, 0, 0))`
///
/// ```eas
/// ;; Extract target address from calldata[0:20].
/// push1 0x00     ;; [0]                                      — calldata offset
/// calldataload   ;; [cd[0..32]]                              — load 32-byte word
/// push1 0x60     ;; [96, cd[0..32]]                          — shift amount
/// shr            ;; [addr]                                   — isolate 20-byte address
///
/// ;; Build CALL args (reverse order — EVM is stack-based).
/// push1 0x00     ;; [0, addr]                                — retLength
/// push1 0x00     ;; [0, 0, addr]                             — retOffset
/// push1 0x00     ;; [0, 0, 0, addr]                          — argsLength
/// push1 0x00     ;; [0, 0, 0, 0, addr]                       — argsOffset
/// callvalue      ;; [value, 0, 0, 0, 0, addr]                — msg.value
/// dup6           ;; [addr, value, 0, 0, 0, 0, addr]          — copy target
/// gas            ;; [gas, addr, value, 0, 0, 0, 0, addr]     — remaining gas
/// call           ;; [success, addr]                           — call(gas, addr, value, 0, 0, 0, 0)
/// pop            ;; [addr]                                    — discard success flag
/// stop           ;; []                                        — always succeed
/// ```
pub fn call_target_with_value_contract_deploy_code() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH1, 0x00, CALLDATALOAD,  // calldataload(0) → 32-byte word
        PUSH1, 0x60, SHR,           // shr(96) → target address

        PUSH1, 0x00,                // call(gas(),
        PUSH1, 0x00,                //   addr,
        PUSH1, 0x00,                //   callvalue(),
        PUSH1, 0x00,                //   0, 0, 0, 0)
        CALLVALUE,                   //
        DUP6,                        // ↑ addr from stack position 6
        GAS,                         //
        CALL,                        // → success
        POP,                         // discard success — always succeed
        STOP,
    ];
    deploy_code(&runtime)
}

/// Contract making `calls` zero-gas CALLs to `target`, each passing memory[0..args_len].
///
/// Pseudocode, repeated `calls` times: `pop(call(0, target, 0, 0, args_len, 0, 0))`
pub fn repeated_call_contract_deploy_code(target: Address, calls: usize, args_len: u16) -> Bytes {
    let [len_hi, len_lo] = args_len.to_be_bytes();
    let mut runtime = Vec::new();
    for _ in 0..calls {
        #[rustfmt::skip]
        runtime.extend_from_slice(&[
            PUSH1, 0x00,              // retLength
            PUSH1, 0x00,              // retOffset
            PUSH2, len_hi, len_lo,    // argsLength
            PUSH1, 0x00,              // argsOffset
            PUSH1, 0x00,              // value
            PUSH20,                   // target address follows
        ]);
        runtime.extend_from_slice(target.as_slice());
        runtime.extend_from_slice(&[PUSH1, 0x00, CALL, POP]); // gas=0
    }
    runtime.push(STOP);
    deploy_code(&runtime)
}

/// Contract that sends `amount` to `recipient`, burns gas down to a 1000-gas floor, then
/// returns `size` zero bytes, where `size` is the 32-byte word in calldata[0..32] (>= 32).
///
/// Memory is expanded before the transfer and the burn, so the final RETURN costs no
/// memory gas and the call succeeds with little gas left: the caller then pays for the
/// returndata with its own retained gas.
///
/// ```eas
/// push1 0x00       ;; [0]                       — calldata offset
/// calldataload     ;; [size]
/// push1 0x00       ;; [0, size]                 — value to store
/// push1 0x20       ;; [32, 0, size]
/// dup3             ;; [size, 32, 0, size]
/// sub              ;; [size-32, 0, size]
/// mstore           ;; [size]                    — mstore(size-32, 0): expand memory
/// push1 0x00 (x4)  ;; [0, 0, 0, 0, size]        — retLength, retOffset, argsLength, argsOffset
/// push8 amount     ;; [amount, 0, 0, 0, 0, size]
/// push20 recipient ;; [recipient, amount, 0, 0, 0, 0, size]
/// gas              ;; [gas, recipient, amount, 0, 0, 0, 0, size]
/// call             ;; [success, size]
/// push1 0x39       ;; [loop, success, size]
/// jumpi            ;; [size]                    — success → loop, else fall through
/// push1 0x00       ;; [0, size]
/// dup1             ;; [0, 0, size]
/// revert           ;;                           — revert(0, 0): transfer failed
/// jumpdest         ;; [size]                    — loop (offset 0x39)
/// gas              ;; [gas, size]
/// push2 0x03e8     ;; [1000, gas, size]
/// lt               ;; [1000 < gas, size]
/// push1 0x39       ;; [loop, 1000 < gas, size]
/// jumpi            ;; [size]                    — keep burning while gas > 1000
/// push1 0x00       ;; [0, size]
/// return           ;;                           — return(0, size)
/// ```
pub fn transfer_burn_gas_return_contract_deploy_code(recipient: Address, amount: u64) -> Bytes {
    let mut runtime = Vec::new();
    #[rustfmt::skip]
    runtime.extend_from_slice(&[
        PUSH1, 0x00, CALLDATALOAD,      // size
        PUSH1, 0x00, PUSH1, 0x20, DUP3,
        SUB, MSTORE,                    // mstore(size-32, 0)
        PUSH1, 0x00,                    // retLength
        PUSH1, 0x00,                    // retOffset
        PUSH1, 0x00,                    // argsLength
        PUSH1, 0x00,                    // argsOffset
        PUSH8,                          // amount follows
    ]);
    runtime.extend_from_slice(&amount.to_be_bytes());
    runtime.push(PUSH20); // recipient follows
    runtime.extend_from_slice(recipient.as_slice());
    #[rustfmt::skip]
    runtime.extend_from_slice(&[
        GAS, CALL,                      // call(gas, recipient, amount, 0, 0, 0, 0)
        PUSH1, 0x39, JUMPI,             // success → loop
        PUSH1, 0x00, DUP1, REVERT,      // revert(0, 0)
        JUMPDEST,                       // loop (offset 0x39)
        GAS, PUSH2, 0x03, 0xe8, LT,     // 1000 < gas
        PUSH1, 0x39, JUMPI,
        PUSH1, 0x00, RETURN,            // return(0, size)
    ]);
    assert_eq!(runtime[0x39], JUMPDEST, "loop offset drifted");
    deploy_code(&runtime)
}

/// Contract that attempts CREATE2 with value=1, then checks the balance of an
/// expected CREATE2 address passed in calldata.
/// - stores the CREATE2 return value in slot 0.
/// - stores the balance + swap1 + pop gas cost in slot 1.
///
/// The calldata address is expected as a right-padded 32-byte word.
///
/// ```eas
/// push32 0x00..00  ;; [salt]            — salt = 0
/// push1 0x01       ;; [1, salt]         — value = 1
/// push1 0x00       ;; [0, 1, salt]      — offset = 0 (mem[0] is 0x00 by default)
/// push1 0x01       ;; [1, 0, 1, salt]   — initcode length = 1
/// create2          ;; [addr]            — create2(value=1, offset=0, len=1, salt=0)
/// push1 0x00       ;; [0, addr]
/// sstore           ;; []                — sstore(0, addr)
/// push1 0x00       ;; [0]               — calldata offset
/// calldataload     ;; [cd[0..32]]       — load expected address word
/// push1 0x60       ;; [96, cd[0..32]]   — shift amount
/// shr              ;; [expected_addr]   — isolate expected address
/// gas              ;; [gas before, expected_addr]
/// swap1            ;; [expected_addr, gas before]
/// balance          ;; [balance, gas before]
/// pop              ;; [gas before]
/// gas              ;; [gas after, gas before]
/// swap1            ;; [gas before, gas after]
/// sub              ;; [gas cost]
/// push1 0x01       ;; [1, gas cost]
/// sstore           ;; []               - sstore(1, gas cost)
///
/// ```
pub fn create2_with_balance_probe() -> Bytes {
    #[rustfmt::skip]
    let runtime = [
        PUSH32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        PUSH1, 0x01,
        PUSH1, 0x00,
        PUSH1, 0x01,
        CREATE2,      // create2(value=1, offset=0, len=1, salt=0)
        PUSH1, 0x00,
        SSTORE,       // sstore(0, CREATE2 return value)
        PUSH1, 0x00,
        CALLDATALOAD,
        PUSH1, 0x60,
        SHR,
        GAS,
        SWAP1,
        BALANCE,
        POP,
        GAS,
        SWAP1,
        SUB,
        PUSH1, 0x01,
        SSTORE,       // sstore(1, gas cost)
    ];
    deploy_code(&runtime)
}

/// Creates calldata for the create2 balance probe contract.
pub fn create2_balance_probe_calldata(creator: Address) -> Bytes {
    right_pad_address(creator.create2([0u8; 32], keccak256([0u8])))
}

/// Right-pads an address to 32 bytes for use as calldata.
///
/// Contracts that read a target address via `CALLDATALOAD(0) + SHR(96)` expect
/// the address in the top 20 bytes of the 32-byte word (right-padded with zeros).
pub fn right_pad_address(addr: alloy_primitives::Address) -> Bytes {
    let mut buf = [0u8; 32];
    buf[..20].copy_from_slice(addr.as_slice());
    Bytes::from(buf.to_vec())
}

/// Helper: wraps runtime bytecode in a minimal constructor that deploys it.
///
/// ```eas
/// ;; Constructor (11 bytes) — copies runtime to memory and returns it.
/// push1 LL       ;; [len]            — runtime bytecode length
/// dup1           ;; [len, len]       — duplicate for RETURN size
/// push1 0x0b     ;; [11, len, len]   — code offset (constructor is 11 bytes)
/// push1 0x00     ;; [0, 11, len, len] — memory destination offset
/// codecopy       ;; [len]            — mem[0..len] = code[11..11+len]
/// push1 0x00     ;; [0, len]         — memory offset for RETURN
/// return         ;; []               — return(0, len) → deployed runtime
/// ```
fn deploy_code(runtime: &[u8]) -> Bytes {
    let len = runtime.len();
    assert!(len < 256, "runtime bytecode too large for PUSH1");
    let constructor_len: u8 = 11;
    let len_u8 = u8::try_from(len).expect("runtime len checked < 256");
    let mut code = Vec::with_capacity(
        usize::from(constructor_len)
            .checked_add(len)
            .expect("total len overflow"),
    );
    #[rustfmt::skip]
    code.extend_from_slice(&[
        PUSH1, len_u8,            // codecopy(0,
        DUP1,                     //
        PUSH1, constructor_len,   //   11,
        PUSH1, 0x00,              //   len)
        CODECOPY,                 //
        PUSH1, 0x00,              // return(0, len)
        RETURN,                   //
    ]);
    code.extend_from_slice(runtime);
    Bytes::from(code)
}
