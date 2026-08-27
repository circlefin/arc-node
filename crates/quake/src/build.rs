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

use std::collections::HashSet;

use crate::setup;

/// Check if a Docker image should be built locally.
///
/// Only `ghcr.io/...` references are treated as pre-built remote images and will be pulled.
/// All other references (e.g. `arc_execution:latest`, `arc_execution:v1.0`) are considered
/// local Quake build targets and will be built from source via Docker Compose.
fn should_build_locally(image: &str) -> bool {
    !image.starts_with("ghcr.io/")
}

/// Compose service name for the nth local build of a layer. The first build keeps
/// the historical base name so single-image scenarios generate identical output.
fn build_service_name(base: &str, index: usize) -> String {
    if index == 0 {
        base.to_string()
    } else {
        format!("{base}_{index}")
    }
}

/// Local build targets for one layer: every distinct locally-built node image,
/// plus the layer's upgrade image, each with a unique compose service name.
/// `images` may contain duplicates and remote (ghcr.io) references; both are skipped.
fn local_builds(
    images: &[String],
    upgrade: Option<&String>,
    base_name: &str,
    upgrade_name: &str,
) -> Vec<setup::ImageBuild> {
    let mut builds = Vec::new();
    let mut seen = HashSet::new();
    for tag in images {
        if should_build_locally(tag) && seen.insert(tag.clone()) {
            builds.push(setup::ImageBuild {
                service_name: build_service_name(base_name, builds.len()),
                tag: tag.clone(),
            });
        }
    }
    if let Some(tag) = upgrade {
        if should_build_locally(tag) && seen.insert(tag.clone()) {
            builds.push(setup::ImageBuild {
                service_name: upgrade_name.to_string(),
                tag: tag.clone(),
            });
        }
    }
    builds
}

/// Build lists of local Docker images to build (excluding remote images), covering
/// every distinct per-node image plus the global upgrade images.
pub(crate) fn local_images_to_build(
    el_images: &[String],
    cl_images: &[String],
    el_upgrade: Option<&String>,
    cl_upgrade: Option<&String>,
) -> (Vec<setup::ImageBuild>, Vec<setup::ImageBuild>) {
    (
        local_builds(
            el_images,
            el_upgrade,
            "arc_execution_build",
            "arc_execution_upgrade_build",
        ),
        local_builds(
            cl_images,
            cl_upgrade,
            "arc_consensus_build",
            "arc_consensus_upgrade_build",
        ),
    )
}

/// Return the distinct remote (ghcr.io) Docker images that need to be pulled from
/// the given set of all effective images.
pub(crate) fn remote_images_to_pull(images: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    images
        .iter()
        .filter(|img| !should_build_locally(img))
        .filter(|img| seen.insert(img.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(builds: &[setup::ImageBuild]) -> Vec<&str> {
        builds.iter().map(|b| b.tag.as_str()).collect()
    }

    #[test]
    fn test_should_build_locally() {
        // Non-GHCR images are built locally
        assert!(should_build_locally("arc_execution:latest"));
        assert!(should_build_locally("arc_consensus:latest"));
        assert!(should_build_locally("arc_execution:v1.0"));
        assert!(should_build_locally("nginx:1.25"));
        assert!(should_build_locally("myimage"));

        // GHCR images are pulled from the registry
        assert!(!should_build_locally("ghcr.io/org-name/image:v1.0"));
        assert!(!should_build_locally(
            "ghcr.io/org-name/repo-name/image:latest"
        ));
    }

    #[test]
    fn test_remote_images_to_pull_all_local() {
        let images = [
            "arc_consensus:latest".to_string(),
            "arc_execution:latest".to_string(),
        ];
        assert!(remote_images_to_pull(&images).is_empty());
    }

    #[test]
    fn test_remote_images_to_pull_mixed() {
        let images = [
            "ghcr.io/org-name/repo-name/cl-image:0.5.0-rc1".to_string(),
            "ghcr.io/org-name/repo-name/el-image:0.5.0-rc1".to_string(),
            "arc_consensus:latest".to_string(),
            "arc_execution:latest".to_string(),
        ];
        assert_eq!(
            remote_images_to_pull(&images),
            vec![
                "ghcr.io/org-name/repo-name/cl-image:0.5.0-rc1",
                "ghcr.io/org-name/repo-name/el-image:0.5.0-rc1",
            ]
        );
    }

    #[test]
    fn remote_images_to_pull_includes_per_node_ghcr_override() {
        // A per-node ghcr.io override must be pulled, not only the global images.
        let images = [
            "arc_consensus:latest".to_string(),
            "arc_execution:latest".to_string(),
            "ghcr.io/org-name/repo-name/el-image:0.6.0".to_string(),
        ];
        assert_eq!(
            remote_images_to_pull(&images),
            vec!["ghcr.io/org-name/repo-name/el-image:0.6.0"]
        );
    }

    #[test]
    fn local_images_to_build_covers_per_node_overrides() {
        // Two distinct local EL images produce two builds with unique service names;
        // the historical base name is preserved for the first.
        let el = [
            "arc_execution:latest".to_string(),
            "arc_execution:old".to_string(),
        ];
        let cl = ["arc_consensus:latest".to_string()];
        let (reth, malachite) = local_images_to_build(&el, &cl, None, None);

        assert_eq!(
            tags(&reth),
            vec!["arc_execution:latest", "arc_execution:old"]
        );
        assert_eq!(reth[0].service_name, "arc_execution_build");
        let names: HashSet<&str> = reth.iter().map(|b| b.service_name.as_str()).collect();
        assert_eq!(names.len(), 2, "service names must be unique");
        assert_eq!(tags(&malachite), vec!["arc_consensus:latest"]);
    }

    #[test]
    fn local_images_to_build_skips_ghcr_and_dedupes() {
        // ghcr images are pulled, not built; duplicates collapse to one build.
        let el = [
            "arc_execution:latest".to_string(),
            "arc_execution:latest".to_string(),
            "ghcr.io/org/el:0.6.0".to_string(),
        ];
        let cl = ["arc_consensus:latest".to_string()];
        let (reth, _) = local_images_to_build(&el, &cl, None, None);
        assert_eq!(tags(&reth), vec!["arc_execution:latest"]);
    }

    #[test]
    fn local_images_to_build_includes_upgrade_images() {
        let el = ["arc_execution:latest".to_string()];
        let cl = ["arc_consensus:latest".to_string()];
        let el_up = "arc_execution:next".to_string();
        let (reth, _) = local_images_to_build(&el, &cl, Some(&el_up), None);
        assert_eq!(
            tags(&reth),
            vec!["arc_execution:latest", "arc_execution:next"]
        );
        assert_eq!(reth[1].service_name, "arc_execution_upgrade_build");
    }
}
