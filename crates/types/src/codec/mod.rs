// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

pub use malachitebft_codec::{Codec, HasEncodedLen};

/// Shared macro for implementing versioned codecs.
///
/// Encoding: Adds a version byte prefix to all encoded messages.
/// Decoding: Reads the version byte prefix and decodes the rest as protobuf.
///
/// # Parameters
/// - `$codec_ty`: The codec type to implement for (e.g., `NetCodec`, `WalCodec`)
/// - `$ty`: The message type to encode/decode
/// - `$version_ty`: The version enum type
/// - `$version_val`: The specific version value to use
macro_rules! impl_versioned_codec {
    ($codec_ty:ty, $ty:ty, $version_ty:ty, $version_val:expr) => {
        impl malachitebft_codec::Codec<$ty> for $codec_ty {
            type Error = $crate::codec::error::CodecError;

            fn decode(&self, mut bytes: bytes::Bytes) -> Result<$ty, Self::Error> {
                use bytes::Buf;

                if bytes.is_empty() {
                    return Err($crate::codec::error::CodecError::EmptyBytes);
                }

                // Fast path: the first byte matches the version we expect
                // for a versioned message. `0x01` cannot start a real
                // protobuf-encoded Arc message (the smallest protobuf
                // field tag with a non-zero field number is `0x08`), so
                // this check is unambiguous: a leading `0x01` byte
                // always means a V1 versioned message. Skipping the
                // legacy decode attempt below saves the cost of a
                // full protobuf parse on every consensus message, which
                // is the common case once all nodes have been upgraded.
                //
                // The legacy "try-raw-protobuf-first" branch is kept
                // below for backward compatibility with messages from
                // pre-versioning nodes; that branch is the only one
                // exercised by the existing `test_previous_codec_compatibility`
                // tests in `wal.rs` and `network.rs`.
                if bytes[0] == $version_val as u8 {
                    let _ = bytes.get_u8();
                    return malachitebft_codec::Codec::decode(
                        &$crate::codec::proto::ProtobufCodec,
                        bytes,
                    )
                    .map_err($crate::codec::error::CodecError::Protobuf);
                }

                // TODO: Phase 3: Remove after all nodes are upgraded to use versioning
                if let Ok(msg) = malachitebft_codec::Codec::decode(
                    &$crate::codec::proto::ProtobufCodec,
                    bytes.clone(),
                ) {
                    return Ok(msg);
                }

                let version_byte = bytes.get_u8();
                let version = <$version_ty>::try_from(version_byte)
                    .map_err($crate::codec::error::CodecError::UnsupportedVersion)?;
                if version != $version_val {
                    return Err($crate::codec::error::CodecError::UnsupportedVersion(
                        version_byte,
                    ));
                }

                malachitebft_codec::Codec::decode(&$crate::codec::proto::ProtobufCodec, bytes)
                    .map_err($crate::codec::error::CodecError::Protobuf)
            }

            fn encode(&self, msg: &$ty) -> Result<bytes::Bytes, Self::Error> {
                use bytes::BufMut;

                let encoded =
                    malachitebft_codec::Codec::encode(&$crate::codec::proto::ProtobufCodec, msg)
                        .map_err($crate::codec::error::CodecError::Protobuf)?;

                #[allow(clippy::arithmetic_side_effects)] // 1 + valid allocation length
                let mut result = bytes::BytesMut::with_capacity(1 + encoded.len());
                result.put_u8($version_val as u8);
                result.put(encoded);

                Ok(result.freeze())
            }
        }
    };
}

pub(crate) use impl_versioned_codec;

pub mod error;
pub mod network;
pub mod proto;
pub mod versions;
pub mod wal;
