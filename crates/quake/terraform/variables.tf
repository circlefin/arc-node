variable "testnet_dir" {
  type = string
}

variable "manifest_path" {
  type = string
}

variable "vpc_cidr" {
  description = "CIDR block for the VPC"
  type        = string
  default     = "172.16.0.0/16"
}

variable "node_names" {
  type    = list(string)
  default = []
}

# Network topology: map of network names to list of node names in that network
# Example: { "trusted" = ["validator1", "validator2"], "default" = ["full1"] }
variable "network_topology" {
  description = "Map of network names to list of node names belonging to that network"
  type        = map(list(string))
  default     = {}
}

variable "github_user" {
  description = "GitHub user that owns the PAT used to pull container images from GHCR"
  type        = string
}
variable "github_token" {
  description = "GitHub PAT used to pull container images from GHCR"
  type        = string
  sensitive   = true
}

variable "image_cl" {
  type = string
}

variable "image_el" {
  type = string
}

variable "region" {
  type    = string
  default = "us-east-1"
}

# EC2 instance type for nodes. Override via `quake remote create --node-size`.
# t3.medium (4 GiB) supports ~12h testnets; t3.large (8 GiB) for day-long runs.
# See README "Instance sizing" for details.
variable "node_size" {
  type    = string
  default = "t3.medium" # 2 vCPUs, 4 GiB RAM
}

# EC2 instance type for the Control Center. Override via `quake remote create --cc-size`.
# See README "Instance sizing" for details.
variable "cc_size" {
  type    = string
  default = "t3.xlarge" # 4 vCPUs, 16 GiB RAM
}

# Root EBS volume size (GiB) for node instances. Override via `quake remote create --node-disk-gb`.
# When null (default), the AMI root volume size is unchanged.
variable "node_volume_size" {
  type     = number
  default  = null
  nullable = true
}

# Root EBS volume size (GiB) for the Control Center. Override via `quake remote create --cc-disk-gb`.
# When null (default), the AMI root volume size is unchanged.
variable "cc_volume_size" {
  type     = number
  default  = null
  nullable = true
}

# Root EBS volume type for nodes (e.g. "gp3", "io2"). Override via `quake remote create --node-volume-type`.
# When null (default), gp3 is used whenever a custom volume_size or iops is set; otherwise the
# AMI default volume type is used.
variable "node_volume_type" {
  type     = string
  default  = null
  nullable = true
}

# Provisioned IOPS for the node root EBS volume. Override via `quake remote create --node-volume-iops`.
# Only meaningful for gp3, io1, and io2. When null (default), AWS uses the volume-type default IOPS.
variable "node_volume_iops" {
  type     = number
  default  = null
  nullable = true
}

# Place the node data directory on local instance-store NVMe instead of the root EBS volume.
# Override via `quake remote create --node-data-on-instance-store`. Requires an instance type
# with local NVMe (e.g. i4i.*, i3.*, m6id.*); a no-op on instance types without instance store.
variable "node_data_on_instance_store" {
  type    = bool
  default = false
}

variable "tags" {
  type    = list(string)
  default = ["arc-quake-testnet"]
}

variable "blockscout_ssm_port" {
  type    = number
  default = 8000
}

variable "circle_base_image" {
  description = "ECR image used as the base layer for spammer/proxy containers"
  type        = string
  sensitive   = true
}

variable "ami_owner" {
  description = "AWS account ID owning the EKS AMI used for node instances"
  type        = string
  sensitive   = true
}

variable "ami_name_filter" {
  type = string
}

variable "ec2_profile_name" {
  description = "IAM instance profile attached to the EC2 nodes"
  type        = string
  sensitive   = true
}
