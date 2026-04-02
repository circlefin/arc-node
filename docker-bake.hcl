// This is set automatically when running in Github Actions
variable "CI" { default = "false" }
// Git version information
variable "GIT_COMMIT_HASH" { default = "unknown" }
variable "GIT_VERSION" { default = "unknown" }
variable "GIT_SHORT_HASH" { default = "unknown" }

variable "IMAGES" {
  default = [
    { name = "execution" },
    { name = "consensus" },
  ]
}

group "default" {
  targets = [for image in IMAGES : "arc-${image.name}"]
}

target "docker-metadata-action" {}

target "meta-target" {
  matrix = { item = IMAGES }

  name      = "${item.name}-meta-target"
  platforms = CI ? ["linux/amd64", "linux/arm64"] : []
  output    = CI ? [] : ["type=docker"]
  tags = [
    # Will be overridden in CI
    "arc/${item.name}:latest",
  ]

  context    = "."
  dockerfile = "deployments/Dockerfile.${item.name}"
  target     = "release-runtime"

  contexts = {
    certs = "./deployments/certs"
  }

  args = {
    GIT_COMMIT_HASH = GIT_COMMIT_HASH
    GIT_VERSION     = GIT_VERSION
    GIT_SHORT_HASH  = GIT_SHORT_HASH
  }
}

target "target" {
  matrix = { item = IMAGES }

  name     = "arc-${item.name}"
  inherits = ["${item.name}-meta-target", "docker-metadata-action"]
}

target "dev-meta-target" {
  matrix = { item = IMAGES }

  name     = "${item.name}-dev-meta-target"
  inherits = ["${item.name}-meta-target"]
  target   = "dev-runtime"
  tags = [
    "arc/${item.name}-dev:latest",
  ]
}

target "dev-target" {
  matrix = { item = IMAGES }

  name     = "arc-${item.name}-dev"
  inherits = ["${item.name}-dev-meta-target", "docker-metadata-action"]
}
