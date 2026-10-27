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

//! ABI decoding of precompile call arguments without copying them out of the input.
//!
//! Lives here rather than in `arc-precompiles` so `arc-pq-precompile`, which `arc-precompiles`
//! depends on, can use it too.
//!
//! alloy's validated decode, [`SolCall::abi_decode_raw_validate`], runs three steps: tokenize,
//! `type_check`, then `detokenize`. Tokenizing reads the offset and length words, bounds-checks
//! them, and yields tokens that borrow from the input. `detokenize` then copies every `bytes`
//! argument into an owned `Bytes`, so a precompile that decodes this way does work proportional
//! to its input before it can charge gas for it.
//!
//! [`abi_decode_raw_borrowed`] runs the same tokenize and `type_check` steps, so it accepts the
//! same inputs and fails with the same errors. In place of `detokenize` it calls
//! [`SolBorrow::borrow_token`], which returns each `bytes` argument as a slice of the input. The
//! caller sees every argument length before anything is copied, can charge for it first, and
//! then copies only what it keeps.
//!
//! ```
//! use alloy_sol_types::{sol, SolCall};
//! use arc_shared::precompile::abi_decode_borrowed;
//!
//! sol! {
//!     function verify(bytes key, bytes message) external returns (bool);
//! }
//!
//! let calldata = verifyCall { key: vec![7; 32].into(), message: b"hello".to_vec().into() }
//!     .abi_encode();
//! let (key, message) = abi_decode_borrowed::<verifyCall>(&calldata)?;
//! assert_eq!(key.len(), 32);
//! assert_eq!(message, b"hello".as_slice());
//! # Ok::<(), alloy_sol_types::Error>(())
//! ```

use alloy_sol_types::abi;
use alloy_sol_types::sol_data::{self, ByteCount, IntBitCount, SupportedFixedBytes, SupportedInt};
use alloy_sol_types::{Error, Result, SolCall, SolType};

#[doc(hidden)]
pub mod __private {
    pub use alloy_sol_types::SolType;
}

/// A Solidity type whose decoded value can borrow from the ABI-encoded input.
///
/// `bytes` decodes to `&[u8]`. Static types decode to their usual Rust value, which is a fixed
/// number of 32-byte words. Tuples of up to 6 elements decode element by element, so the
/// parameter list of a call decodes to a tuple in declaration order.
///
/// `string`, arrays, and structs with dynamic fields have no impl yet; add one when a
/// precompile takes such an argument. A static struct can use [`impl_sol_borrow_by_value!`].
pub trait SolBorrow: SolType {
    /// The decoded value, which borrows from the input for dynamic types.
    type Borrowed<'a>;

    /// Converts a token into its decoded value.
    ///
    /// Like [`SolType::detokenize`], this does not validate the token. Type-check it first, as
    /// [`abi_decode_raw_borrowed`] does.
    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a>;
}

/// Implements [`SolBorrow`] for static Solidity types by decoding them to their Rust value.
///
/// A static value is a fixed number of 32-byte words, so decoding it by value copies nothing
/// proportional to the input. The macro fails to compile for a dynamic type.
#[macro_export]
macro_rules! impl_sol_borrow_by_value {
    ($($ty:ty),+ $(,)?) => {$(
        const _: () = assert!(
            !<$ty as $crate::precompile::__private::SolType>::DYNAMIC,
            "impl_sol_borrow_by_value! only accepts static Solidity types",
        );

        impl $crate::precompile::SolBorrow for $ty {
            type Borrowed<'a> = <Self as $crate::precompile::__private::SolType>::RustType;

            fn borrow_token<'a>(
                token: <Self as $crate::precompile::__private::SolType>::Token<'a>,
            ) -> Self::Borrowed<'a> {
                <Self as $crate::precompile::__private::SolType>::detokenize(token)
            }
        }
    )+};
}

impl_sol_borrow_by_value!(sol_data::Address, sol_data::Bool);

impl<const BITS: usize> SolBorrow for sol_data::Uint<BITS>
where
    IntBitCount<BITS>: SupportedInt,
{
    type Borrowed<'a> = <Self as SolType>::RustType;

    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
        Self::detokenize(token)
    }
}

impl<const BITS: usize> SolBorrow for sol_data::Int<BITS>
where
    IntBitCount<BITS>: SupportedInt,
{
    type Borrowed<'a> = <Self as SolType>::RustType;

    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
        Self::detokenize(token)
    }
}

impl<const N: usize> SolBorrow for sol_data::FixedBytes<N>
where
    ByteCount<N>: SupportedFixedBytes,
{
    type Borrowed<'a> = <Self as SolType>::RustType;

    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
        Self::detokenize(token)
    }
}

impl SolBorrow for sol_data::Bytes {
    type Borrowed<'a> = &'a [u8];

    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
        token.0
    }
}

impl SolBorrow for () {
    type Borrowed<'a> = ();

    fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
        token
    }
}

macro_rules! impl_tuple {
    ($($ty:ident),+) => {
        #[allow(non_snake_case)]
        impl<$($ty: SolBorrow,)+> SolBorrow for ($($ty,)+) {
            type Borrowed<'a> = ($($ty::Borrowed<'a>,)+);

            fn borrow_token<'a>(token: Self::Token<'a>) -> Self::Borrowed<'a> {
                let ($($ty,)+) = token;
                ($(<$ty as SolBorrow>::borrow_token($ty),)+)
            }
        }
    };
}

impl_tuple!(T1);
impl_tuple!(T1, T2);
impl_tuple!(T1, T2, T3);
impl_tuple!(T1, T2, T3, T4);
impl_tuple!(T1, T2, T3, T4, T5);
impl_tuple!(T1, T2, T3, T4, T5, T6);

/// The arguments of call `C` as the borrowed decoders return them: a tuple of its parameters in
/// declaration order, with `bytes` parameters as slices of the input.
pub type BorrowedArgs<'a, C> = <<C as SolCall>::Parameters<'a> as SolBorrow>::Borrowed<'a>;

/// ABI-decodes the arguments of call `C` from `data`, the calldata after the selector, without
/// copying them.
///
/// Accepts the same inputs as [`SolCall::abi_decode_raw_validate`] and fails with the same
/// errors: both tokenize with [`abi::decode_sequence`] and then `type_check`. The only
/// difference is the result: `bytes` arguments are slices of `data` instead of owned copies.
pub fn abi_decode_raw_borrowed<'a, C>(data: &'a [u8]) -> Result<BorrowedArgs<'a, C>>
where
    C: SolCall,
    C::Parameters<'a>: SolBorrow,
{
    let tokens = abi::decode_sequence::<C::Token<'a>>(data)?;
    <C::Parameters<'a> as SolType>::type_check(&tokens)?;
    Ok(<C::Parameters<'a> as SolBorrow>::borrow_token(tokens))
}

/// Same as [`abi_decode_raw_borrowed`], for calldata that starts with the selector of `C`.
///
/// Accepts the same inputs as [`SolCall::abi_decode_validate`] and fails with the same errors,
/// including the error for a missing or different selector.
pub fn abi_decode_borrowed<'a, C>(data: &'a [u8]) -> Result<BorrowedArgs<'a, C>>
where
    C: SolCall,
    C::Parameters<'a>: SolBorrow,
{
    let body = data
        .strip_prefix(&C::SELECTOR)
        .ok_or_else(|| Error::type_check_fail_sig(data, C::SIGNATURE))?;
    abi_decode_raw_borrowed::<C>(body)
}

#[cfg(test)]
mod tests {
    use super::{abi_decode_borrowed, abi_decode_raw_borrowed};
    use alloy_primitives::{Address, Bytes, B256, I256, U256};
    use alloy_sol_types::{sol, SolCall};
    use proptest::prelude::*;
    use proptest::sample::Index;

    sol! {
        #[derive(Debug, PartialEq, Eq)]
        struct StaticValues {
            uint64 count;
            address account;
            bool flag;
        }

        interface IDecodeTest {
            function noArgs() external;
            function takesAddress(address account) external;
            function takesUint64(uint64 value) external;
            function threeBytes(bytes vk, bytes message, bytes sig) external;
            function callFrom(address sender, address target, bytes data) external;
            function mixed(bool flag, uint8 small, int256 signed, bytes32 word, bytes payload) external;
            function withStruct(uint64 blockNumber, StaticValues values) external;
        }
    }

    crate::impl_sol_borrow_by_value!(StaticValues);

    use IDecodeTest::{
        callFromCall, mixedCall, noArgsCall, takesAddressCall, takesUint64Call, threeBytesCall,
        withStructCall,
    };

    fn encode_raw(call: &impl SolCall) -> Vec<u8> {
        let mut out = Vec::new();
        call.abi_encode_raw(&mut out);
        out
    }

    fn bytes_arg() -> impl Strategy<Value = Bytes> + Clone {
        prop::collection::vec(any::<u8>(), 0..=96).prop_map(Bytes::from)
    }

    fn address() -> impl Strategy<Value = Address> + Clone {
        any::<[u8; 20]>().prop_map(Address::from)
    }

    fn three_bytes_body() -> impl Strategy<Value = Vec<u8>> + Clone {
        (bytes_arg(), bytes_arg(), bytes_arg())
            .prop_map(|(vk, message, sig)| encode_raw(&threeBytesCall { vk, message, sig }))
    }

    fn call_from_calldata() -> impl Strategy<Value = Vec<u8>> + Clone {
        (address(), address(), bytes_arg()).prop_map(|(sender, target, data)| {
            callFromCall {
                sender,
                target,
                data,
            }
            .abi_encode()
        })
    }

    fn mixed_body() -> impl Strategy<Value = Vec<u8>> + Clone {
        (
            any::<bool>(),
            any::<u8>(),
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
            bytes_arg(),
        )
            .prop_map(|(flag, small, signed, word, payload)| {
                encode_raw(&mixedCall {
                    flag,
                    small,
                    signed: I256::from_raw(U256::from_be_bytes(signed)),
                    word: B256::from(word),
                    payload,
                })
            })
    }

    fn with_struct_body() -> impl Strategy<Value = Vec<u8>> + Clone {
        (any::<u64>(), any::<u64>(), address(), any::<bool>()).prop_map(
            |(block_number, count, account, flag)| {
                encode_raw(&withStructCall {
                    blockNumber: block_number,
                    values: StaticValues {
                        count,
                        account,
                        flag,
                    },
                })
            },
        )
    }

    /// Words worth writing over a head slot: arbitrary ones, and small ones that stay in bounds
    /// when read as an offset or a length.
    fn word() -> impl Strategy<Value = [u8; 32]> {
        prop_oneof![
            any::<[u8; 32]>(),
            (0u64..=256).prop_map(|v| U256::from(v).to_be_bytes::<32>()),
        ]
    }

    /// `canonical` encodings as they are, then with one 32-byte word overwritten, the tail cut
    /// off, or one byte flipped, plus fully random bytes.
    fn abi_inputs(
        canonical: impl Strategy<Value = Vec<u8>> + Clone + 'static,
    ) -> BoxedStrategy<Vec<u8>> {
        prop_oneof![
            2 => canonical.clone(),
            3 => (canonical.clone(), any::<Index>(), word()).prop_map(|(mut data, index, word)| {
                let words = data.chunks_exact(32).len();
                if words > 0 {
                    if let Some(slot) = data.chunks_exact_mut(32).nth(index.index(words)) {
                        slot.copy_from_slice(&word);
                    }
                }
                data
            }),
            1 => (canonical.clone(), any::<Index>()).prop_map(|(mut data, index)| {
                if !data.is_empty() {
                    data.truncate(index.index(data.len()));
                }
                data
            }),
            1 => (canonical, any::<Index>(), 1u8..).prop_map(|(mut data, index, mask)| {
                if !data.is_empty() {
                    let position = index.index(data.len());
                    data[position] ^= mask;
                }
                data
            }),
            1 => prop::collection::vec(any::<u8>(), 0..=320),
        ]
        .boxed()
    }

    /// Asserts that a borrowed decode agrees with alloy's validated decode of the same input:
    /// both accept with equal values, or both reject with the same message. `|call| expected`
    /// maps alloy's owned call to the borrowed form.
    macro_rules! prop_assert_agrees {
        ($borrowed:expr, $owned:expr, |$call:ident| $expected:expr) => {{
            let borrowed = $borrowed.map_err(|e| e.to_string());
            let owned = $owned.map_err(|e| e.to_string());
            let expected = match &owned {
                Ok($call) => Ok($expected),
                Err(e) => Err(e.clone()),
            };
            prop_assert_eq!(borrowed, expected);
        }};
    }

    proptest! {
        #[test]
        fn no_args_agrees_with_alloy(data in prop::collection::vec(any::<u8>(), 0..=64)) {
            prop_assert_agrees!(
                abi_decode_raw_borrowed::<noArgsCall>(&data),
                noArgsCall::abi_decode_raw_validate(&data),
                |_call| ()
            );
        }

        #[test]
        fn three_bytes_agrees_with_alloy(data in abi_inputs(three_bytes_body())) {
            prop_assert_agrees!(
                abi_decode_raw_borrowed::<threeBytesCall>(&data),
                threeBytesCall::abi_decode_raw_validate(&data),
                |call| (&call.vk[..], &call.message[..], &call.sig[..])
            );
        }

        #[test]
        fn mixed_agrees_with_alloy(data in abi_inputs(mixed_body())) {
            prop_assert_agrees!(
                abi_decode_raw_borrowed::<mixedCall>(&data),
                mixedCall::abi_decode_raw_validate(&data),
                |call| (call.flag, call.small, call.signed, call.word, &call.payload[..])
            );
        }

        #[test]
        fn static_struct_agrees_with_alloy(data in abi_inputs(with_struct_body())) {
            prop_assert_agrees!(
                abi_decode_raw_borrowed::<withStructCall>(&data),
                withStructCall::abi_decode_raw_validate(&data),
                |call| (call.blockNumber, call.values.clone())
            );
        }

        #[test]
        fn with_selector_agrees_with_alloy(data in abi_inputs(call_from_calldata())) {
            prop_assert_agrees!(
                abi_decode_borrowed::<callFromCall>(&data),
                callFromCall::abi_decode_validate(&data),
                |call| (call.sender, call.target, &call.data[..])
            );
        }
    }

    #[test]
    fn bytes_arguments_borrow_from_input() {
        let data = encode_raw(&threeBytesCall {
            vk: vec![1; 32].into(),
            message: vec![2; 100].into(),
            sig: vec![3; 64].into(),
        });
        let (vk, message, sig) =
            abi_decode_raw_borrowed::<threeBytesCall>(&data).expect("canonical encoding");

        assert_eq!(vk, [1; 32].as_slice());
        assert_eq!(message, [2; 100].as_slice());
        assert_eq!(sig, [3; 64].as_slice());
        let input = data.as_ptr_range();
        for arg in [vk, message, sig] {
            let arg = arg.as_ptr_range();
            assert!(
                input.start <= arg.start && arg.end <= input.end,
                "argument was copied out of the input"
            );
        }
    }

    #[test]
    fn rejects_dirty_address_padding() {
        let mut data = [0u8; 32];
        data[..12].fill(0x11);
        data[12] = 0xaa;

        let result = abi_decode_raw_borrowed::<takesAddressCall>(&data);
        assert!(result.is_err(), "non-zero address padding must be rejected");
    }

    #[test]
    fn rejects_dirty_uint64_padding() {
        let mut data = [0u8; 32];
        data[0] = 0x11;
        data[31] = 42;

        let result = abi_decode_raw_borrowed::<takesUint64Call>(&data);
        assert!(result.is_err(), "non-zero uint64 padding must be rejected");
    }
}
