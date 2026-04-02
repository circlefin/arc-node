locals {
  testnet_name = lower(replace(basename(var.testnet_dir), "/[/.]/", "-"))
  project_name = lower(replace("arc-${local.testnet_name}-testnet-${var.github_user}", "/[/.]/", "-"))
  testnet_size = length(var.node_names)

  root_dir                  = "../../.."
  manifest_path             = "${local.root_dir}/${var.manifest_path}"
  testnet_path              = "${local.root_dir}/${var.testnet_dir}"
  private_key_path          = "${local.testnet_path}/ssh-private-key.pem"
  infra_data_local_path     = "${local.root_dir}/${var.testnet_dir}/infra-data.json"
  infra_data_remote_path    = ".quake/testnet/infra-data.json"
  prometheus_yml_local_path = "${local.testnet_path}/monitoring/prometheus.yml"
  deployments_dir           = "${local.root_dir}/deployments"
  username                  = "ssm-user"
  ec2_profile_name          = var.ec2_profile_name
  arch                      = "debian_amd64"

  # Network topology: use provided topology or default all nodes to "default" network
  effective_network_topology = length(var.network_topology) > 0 ? var.network_topology : {
    "default" = var.node_names
  }

  # List of unique network names
  network_names = keys(local.effective_network_topology)

  # Map network name to subnet index (0-based) for CIDR calculation
  network_index_map = { for idx, name in local.network_names : name => idx }

  # For each node, find which networks it belongs to (ordered list)
  node_networks = {
    for node_name in var.node_names : node_name => [
      for net_name in local.network_names : net_name
      if contains(local.effective_network_topology[net_name], node_name)
    ]
  }

  # The first network for each node (used for primary ENI)
  node_primary_network = {
    for node_name, networks in local.node_networks : node_name => networks[0]
  }

  # Additional networks for each node (used for secondary ENIs)
  node_secondary_networks = {
    for node_name, networks in local.node_networks : node_name => slice(networks, 1, length(networks))
  }

  # Build node metadata including all private IPs from all ENIs
  nodes = [
    for idx, node in aws_instance.node : {
      name        = node.tags.Name
      urn         = node.arn
      public_ip   = node.public_ip
      private_ip  = node.private_ip
      instance_id = node.id
      # Map of network name to private IP for this node
      network_ips = merge(
        # Primary ENI IP
        { (local.node_primary_network[var.node_names[idx]]) = node.private_ip },
        # Secondary ENI IPs
        {
          for eni in aws_network_interface.node_secondary_eni :
          eni.tags.network => eni.private_ip
          if eni.tags.node == var.node_names[idx]
        }
      )
    }
  ]

  cc = {
    name        = aws_instance.cc.tags.Name
    public_ip   = aws_instance.cc.public_ip
    private_ip  = aws_instance.cc.private_ip
    network_ips = { (local.network_names[0]) = aws_instance.cc.private_ip }
    instance_id = aws_instance.cc.id
  }

  ssh_opts = "-o StrictHostKeyChecking=no -o LogLevel=ERROR -o \"ProxyCommand=aws ssm start-session --target %h --document-name AWS-StartSSHSession --parameters portNumber=%p\" -i ${local.private_key_path}"
}

# Generate SSH key and register as EC2 key pair
resource "tls_private_key" "ssh" {
  algorithm = "ED25519"
}

resource "aws_key_pair" "testnet_key" {
  key_name   = "${local.project_name}-key"
  public_key = tls_private_key.ssh.public_key_openssh
}

# Save private key locally for SSH
resource "local_file" "testnet_private_key" {
  filename        = local.private_key_path
  content         = tls_private_key.ssh.private_key_openssh
  file_permission = "0600"
}

# Generate Blockscout Postgres password and matching backend env file
resource "random_password" "blockscout_db" {
  length           = 32
  special          = true
  override_special = "-_"
}

resource "aws_vpc" "testnet_vpc" {
  cidr_block           = var.vpc_cidr
  enable_dns_hostnames = true
  enable_dns_support   = true

  tags = {
    Name = local.project_name
  }
}

# Create one subnet per logical network defined in the manifest
# Each subnet gets a /24 CIDR block within the VPC's /16
resource "aws_subnet" "network_subnet" {
  for_each = toset(local.network_names)

  vpc_id                  = aws_vpc.testnet_vpc.id
  cidr_block              = cidrsubnet(var.vpc_cidr, 8, local.network_index_map[each.key])
  availability_zone       = data.aws_availability_zones.available.names[0]
  map_public_ip_on_launch = true

  tags = {
    Name    = "${local.project_name}-${each.key}"
    network = each.key
  }
}

resource "aws_internet_gateway" "testnet_igw" {
  vpc_id = aws_vpc.testnet_vpc.id

  tags = {
    Name = local.project_name
  }
}

resource "aws_route_table" "testnet_rt" {
  vpc_id = aws_vpc.testnet_vpc.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.testnet_igw.id
  }

  tags = {
    Name = local.project_name
  }
}

# Associate route table with all network subnets
resource "aws_route_table_association" "network_rta" {
  for_each = aws_subnet.network_subnet

  subnet_id      = each.value.id
  route_table_id = aws_route_table.testnet_rt.id
}

# Security group per network - only allows traffic within the same network subnet
resource "aws_security_group" "network_sg" {
  for_each = toset(local.network_names)

  name        = "${local.project_name}-${each.key}-sg"
  description = "Security group for ${each.key} network"
  vpc_id      = aws_vpc.testnet_vpc.id

  # Allow all traffic within this network's subnet only
  ingress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [aws_subnet.network_subnet[each.key].cidr_block]
  }

  # Allow all outbound traffic
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name    = "${local.project_name}-${each.key}"
    network = each.key
  }
}

# Allow CC to access all network security groups (for monitoring, SSH, etc.)
resource "aws_security_group_rule" "cc_to_network" {
  for_each = toset(local.network_names)

  type                     = "ingress"
  from_port                = 0
  to_port                  = 0
  protocol                 = "-1"
  security_group_id        = aws_security_group.network_sg[each.key].id
  source_security_group_id = aws_security_group.cc_sg.id
  description              = "Allow CC to access ${each.key} network"
}

# Security group for CC (control center) - can access all networks
resource "aws_security_group" "cc_sg" {
  name        = "${local.project_name}-cc-sg"
  description = "Security group for control center"
  vpc_id      = aws_vpc.testnet_vpc.id

  # Allow all internal traffic within VPC (CC needs access to all nodes)
  ingress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [var.vpc_cidr]
  }

  # Allow all outbound traffic
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "${local.project_name}-cc-sg"
  }
}

# Data source to get available AZs
data "aws_availability_zones" "available" {
  state = "available"
}

# Amazon Linux 2023
data "aws_ami" "amazonlinux_al2023" {
  most_recent = true
  owners      = [var.ami_owner]

  filter {
    name   = "name"
    values = [var.ami_name_filter]
  }
}
