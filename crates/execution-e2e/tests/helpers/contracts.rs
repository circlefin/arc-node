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
//!
//! Each test binary only uses a subset of these; unused items are expected.
#![allow(dead_code)]

use alloy_primitives::Bytes;
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
        PUSH1, len_u8,           // codecopy(0,
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
