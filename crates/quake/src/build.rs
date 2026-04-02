use crate::{setup, testnet};

/// Check if a Docker image should be built locally.
///
/// Only `ghcr.io/...` references are treated as pre-built remote images and will be pulled.
/// All other references (e.g. `arc_execution:latest`, `arc_execution:v1.0`) are considered
/// local Quake build targets and will be built from source via Docker Compose.
fn should_build_locally(image: &str) -> bool {
    !image.starts_with("ghcr.io/")
}

fn push_build_if_local(builds: &mut Vec<setup::ImageBuild>, tag: &str, service_name: &str) {
    if should_build_locally(tag) {
        builds.push(setup::ImageBuild {
            service_name: service_name.to_string(),
            tag: tag.to_string(),
        });
    }
}

fn maybe_push_build(builds: &mut Vec<setup::ImageBuild>, tag: Option<&String>, service_name: &str) {
    if let Some(tag) = tag {
        push_build_if_local(builds, tag, service_name);
    }
}

/// Build lists of local Docker images to build (excluding remote images).
pub(crate) fn local_images_to_build(
    images: &testnet::DockerImages,
) -> (Vec<setup::ImageBuild>, Vec<setup::ImageBuild>) {
    let mut reth_builds = Vec::new();
    let mut malachite_builds = Vec::new();

    push_build_if_local(&mut reth_builds, &images.el, "arc_execution_build");
    push_build_if_local(&mut malachite_builds, &images.cl, "arc_consensus_build");
    maybe_push_build(
        &mut reth_builds,
        images.el_upgrade.as_ref(),
        "arc_execution_upgrade_build",
    );
    maybe_push_build(
        &mut malachite_builds,
        images.cl_upgrade.as_ref(),
        "arc_consensus_upgrade_build",
    );

    (reth_builds, malachite_builds)
}

/// Return the list of remote Docker images that need to be pulled.
pub(crate) fn remote_images_to_pull(images: &testnet::DockerImages) -> Vec<String> {
    images
        .all()
        .into_iter()
        .filter(|img| !should_build_locally(img))
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testnet::DockerImages;

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
        let images = DockerImages {
            cl: "arc_consensus:latest".to_string(),
            el: "arc_execution:latest".to_string(),
            cl_upgrade: None,
            el_upgrade: None,
        };
        let remote = remote_images_to_pull(&images);
        assert!(remote.is_empty());
    }

    #[test]
    fn test_remote_images_to_pull_mixed() {
        let images = DockerImages {
            cl: "ghcr.io/org-name/repo-name/cl-image:0.5.0-rc1".to_string(),
            el: "ghcr.io/org-name/repo-name/el-image:0.5.0-rc1".to_string(),
            cl_upgrade: Some("arc_consensus:latest".to_string()),
            el_upgrade: Some("arc_execution:latest".to_string()),
        };
        let remote = remote_images_to_pull(&images);
        assert_eq!(
            remote,
            vec![
                "ghcr.io/org-name/repo-name/cl-image:0.5.0-rc1",
                "ghcr.io/org-name/repo-name/el-image:0.5.0-rc1",
            ]
        );
    }

    #[test]
    fn test_remote_images_to_pull_all_remote() {
        let images = DockerImages {
            cl: "ghcr.io/org-name/repo-name/cl-image:latest".to_string(),
            el: "ghcr.io/org-name/repo-name/el-image:latest".to_string(),
            cl_upgrade: None,
            el_upgrade: None,
        };
        let remote = remote_images_to_pull(&images);
        assert_eq!(
            remote,
            vec![
                "ghcr.io/org-name/repo-name/cl-image:latest",
                "ghcr.io/org-name/repo-name/el-image:latest",
            ]
        );
    }
}
