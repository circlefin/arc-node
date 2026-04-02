# Each node is placed in its primary network's subnet
resource "aws_instance" "node" {
  count         = local.testnet_size
  ami           = data.aws_ami.amazonlinux_al2023.id
  instance_type = var.node_size
  # Place node in its primary network's subnet
  subnet_id = aws_subnet.network_subnet[local.node_primary_network[var.node_names[count.index]]].id
  key_name  = aws_key_pair.testnet_key.key_name
  # Security group for the primary network
  vpc_security_group_ids = [aws_security_group.network_sg[local.node_primary_network[var.node_names[count.index]]].id]
  iam_instance_profile   = local.ec2_profile_name

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required" # IMDSv2 only
    http_put_response_hop_limit = 1
  }

  user_data = templatefile("nodes-data.yaml", {
    id              = var.node_names[count.index]
    username        = local.username
    region          = var.region
    ssh_public_key  = tls_private_key.ssh.public_key_openssh
    arch            = local.arch,
    consensus_image = var.image_cl
    execution_image = var.image_el
    github_user     = var.github_user
    github_token    = var.github_token
  })

  tags = merge(
    { Name = var.node_names[count.index] },
    { for tag in var.tags : tag => "true" },
    { region = var.region },
    { project = local.project_name },
    { primary_network = local.node_primary_network[var.node_names[count.index]] }
  )
}

# Secondary ENIs for nodes that belong to multiple networks (bridge nodes)
# This creates one ENI per (node, secondary_network) pair
locals {
  # Flatten to list of {node_name, network_name, node_index} for secondary ENIs
  secondary_eni_list = flatten([
    for idx, node_name in var.node_names : [
      for net_name in local.node_secondary_networks[node_name] : {
        node_name  = node_name
        node_index = idx
        network    = net_name
        key        = "${node_name}-${net_name}"
      }
    ]
  ])
  secondary_eni_map = { for item in local.secondary_eni_list : item.key => item }
}

# Create secondary ENIs for bridge nodes
resource "aws_network_interface" "node_secondary_eni" {
  for_each = local.secondary_eni_map

  subnet_id       = aws_subnet.network_subnet[each.value.network].id
  security_groups = [aws_security_group.network_sg[each.value.network].id]

  tags = {
    Name    = "${local.project_name}-${each.value.node_name}-${each.value.network}"
    node    = each.value.node_name
    network = each.value.network
    project = local.project_name
  }
}

# Attach secondary ENIs to their respective instances
resource "aws_network_interface_attachment" "node_secondary_eni_attachment" {
  for_each = local.secondary_eni_map

  instance_id          = aws_instance.node[each.value.node_index].id
  network_interface_id = aws_network_interface.node_secondary_eni[each.key].id
  device_index         = index(local.node_secondary_networks[each.value.node_name], each.value.network) + 1

  depends_on = [aws_instance.node]
}

# Generate prometheus.yml file from template
resource "local_file" "prometheus_yml" {
  depends_on = [aws_instance.node]
  content    = templatefile("templates/monitoring/prometheus-yml.tmpl", { nodes = local.nodes })
  filename   = local.prometheus_yml_local_path
}

# Generate rpc-proxy.conf from template
resource "local_file" "rpc_proxy_conf" {
  depends_on = [aws_instance.node]
  content    = templatefile("templates/rpc-proxy/rpc-proxy-conf.tmpl", { nodes = local.nodes })
  filename   = "${local.testnet_path}/rpc-proxy/rpc-proxy.conf"
}

# Copy rpc-proxy compose.yaml (static file, no templating needed)
resource "local_file" "rpc_proxy_compose" {
  content  = file("templates/rpc-proxy/compose.yaml.hbs")
  filename = "${local.testnet_path}/rpc-proxy/compose.yaml"
}

# Generate pprof-proxy.conf from template
resource "local_file" "pprof_proxy_conf" {
  depends_on = [aws_instance.node]
  content    = templatefile("templates/pprof-proxy/pprof-proxy-conf.tmpl", { nodes = local.nodes })
  filename   = "${local.testnet_path}/pprof-proxy/pprof-proxy.conf"
}

# Copy pprof-proxy compose.yaml (static file, no templating needed)
resource "local_file" "pprof_proxy_compose" {
  content  = file("templates/pprof-proxy/compose.yaml.hbs")
  filename = "${local.testnet_path}/pprof-proxy/compose.yaml"
}

# Generate monitoring compose.yaml from template
# Use try() to handle destroy when nodes are already gone
resource "local_file" "monitoring_compose" {
  depends_on = [aws_instance.node]
  content = templatefile("templates/monitoring/compose.yaml.hbs", {
    some_node_private_ip = try(aws_instance.node[0].private_ip, "127.0.0.1"),
    blockscout_ssm_port  = var.blockscout_ssm_port
  })
  filename = "${local.testnet_path}/monitoring/compose.yaml"
}
