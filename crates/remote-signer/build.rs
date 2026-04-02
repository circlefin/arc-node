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

fn main() {
    use protox::prost::Message;
    use std::path::PathBuf;

    let protos = ["proto/arc/signer/v1/signer.proto"];
    let includes = ["proto"];

    // Compile the proto files
    let fd = protox::compile(protos, includes)
        .expect("Failed to compile protobuf files. Check that proto files exist and are valid.");

    // Get the path to the output directory for the file descriptor set
    let fd_path =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set")).join("proto_fd.bin");

    // Write the file descriptor set to the output directory
    std::fs::write(&fd_path, fd.encode_to_vec()).expect("Failed to write file descriptor set");

    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .file_descriptor_set_path(fd_path)
        .skip_protoc_run()
        .compile_protos(&protos, &includes)
        .expect("Failed to compile gRPC definitions");
}
