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

variable "github_user" {}
variable "github_token" {}

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

variable "tags" {
  type    = list(string)
  default = ["arc-quake-testnet"]
}

variable "blockscout_ssm_port" {
  type    = number
  default = 8000
}

variable "circle_base_image" {
  type = string
}

variable "ami_owner" {
  type = string
}

variable "ami_name_filter" {
  type = string
}

variable "ec2_profile_name" {
  type = string
}
