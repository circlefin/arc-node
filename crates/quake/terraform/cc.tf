# Control Center (CC)
resource "aws_instance" "cc" {
  depends_on = [
    aws_instance.node,
    random_password.blockscout_db,
    aws_subnet.network_subnet,
  ]

  ami           = data.aws_ami.amazonlinux_al2023.id
  instance_type = var.cc_size
  # Place CC in the first network's subnet (arbitrary choice, CC can access all networks)
  subnet_id              = aws_subnet.network_subnet[local.network_names[0]].id
  key_name               = aws_key_pair.testnet_key.key_name
  vpc_security_group_ids = [aws_security_group.cc_sg.id]
  iam_instance_profile   = local.ec2_profile_name

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required" # IMDSv2 only
    http_put_response_hop_limit = 1
  }

  user_data = templatefile("cc-data.yaml", {
    username        = local.username,
    region          = var.region,
    ssh_public_key  = tls_private_key.ssh.public_key_openssh,
    ssh_private_key = tls_private_key.ssh.private_key_openssh,
    arch            = local.arch,
    vpc_cidr        = var.vpc_cidr,
    # Encode to base64 to avoid issues with YAML special characters (:, #, etc.)
    compose_yaml_b64 = base64encode(templatefile("templates/monitoring/compose.yaml.hbs", {
      some_node_private_ip = aws_instance.node[0].private_ip,
      blockscout_ssm_port  = var.blockscout_ssm_port
    })),
    blockscout_db_password_b64 = base64encode(random_password.blockscout_db.result),
  })

  tags = merge(
    { Name = "cc" },
    { for tag in var.tags : tag => "true" },
    { cc = "true" },
    { region = var.region },
    { project = local.project_name }
  )
}

# Auto-recover the CC instance when system status checks fail.
# Uses the EC2 recover action which migrates the instance to a different host,
# preserving the instance ID, private IP, and EBS volumes.
resource "aws_cloudwatch_metric_alarm" "cc_auto_recovery" {
  alarm_name          = "${local.project_name}-cc-auto-recovery"
  namespace           = "AWS/EC2"
  metric_name         = "StatusCheckFailed_System"
  statistic           = "Maximum"
  period              = 60
  evaluation_periods  = 2
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  dimensions = {
    InstanceId = aws_instance.cc.id
  }
  alarm_actions = ["arn:aws:automate:${var.region}:ec2:recover"]
}

# Wait until SSM agent in CC is up, and CC is ready.
resource "null_resource" "cc-ready" {
  depends_on = [aws_instance.cc]

  triggers = {
    cc_instance_id = aws_instance.cc.id
  }

  provisioner "local-exec" {
    command = <<-EOT
    until [ "$(aws ssm get-connection-status --target ${aws_instance.cc.id} --query 'Status' --output text)" == "connected" ]; do
      sleep 1
    done
    ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} "until [ -f /etc/done ]; do sleep 1; done"
    EOT
  }
}


# Mount NFS on nodes and create symlinks (runs from CC after it's ready).
# Depends on spammer-image-upload to limit concurrent SSM sessions to the CC
# instance — without this, all four post-CC provisioners fire at once and the
# SSM agent rejects connections with TargetNotConnected.
resource "null_resource" "nodes-nfs-mount" {
  depends_on = [
    null_resource.cc-ready,
    terraform_data.spammer-image-upload,
  ]

  # From CC, set up NFS on each node in parallel: wait for each node's cloud-init
  # to complete, then add the NFS fstab entry, mount the shared directory, and
  # create symlinks. Note: /shared is also mounted inside containers so symlinks
  # can be resolved.
  provisioner "local-exec" {
    command = <<-EOT
    ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} '
      setup_node() {
        local target="$1"
        local name="$2"
        # Wait for node cloud-init to complete before setting up NFS
        until ssh -o StrictHostKeyChecking=no -o LogLevel=ERROR "$target" "test -f /etc/done" 2>/dev/null; do
          sleep 1
        done
        ssh -o StrictHostKeyChecking=no -o LogLevel=ERROR "$target" "\
          echo '"'"'${aws_instance.cc.private_ip}:/home/${local.username}/shared /shared nfs soft,timeo=50,retrans=3,_netdev 0 0'"'"' | sudo tee -a /etc/fstab && \
          sudo mkdir -p /shared && \
          sudo mount -a && \
          mkdir -p /home/${local.username}/data/malachite /home/${local.username}/data/reth/execution-data && \
          ln -s /shared/assets /home/${local.username}/assets && \
          ln -s /shared/$name/malachite/config /home/${local.username}/data/malachite/config && \
          ln -s /shared/$name/reth/nodekey /home/${local.username}/data/reth/execution-data/nodekey && \
          ln -s /shared/$name/compose.yaml /home/${local.username}/compose.yaml"
      }

      pids=()
      for node in ${join(" ", [for n in local.nodes : "${local.username}@${n.private_ip}:${n.name}"])}; do
        IFS=":" read -r target name <<< "$node"
        setup_node "$target" "$name" &
        pids+=($!)
      done

      # Wait for all background jobs and capture any failures
      failed=0
      for pid in "$${pids[@]}"; do
        if ! wait "$pid"; then
          failed=1
        fi
      done
      exit $failed
    '
    EOT
  }
}

// Provision RPC proxy config files and start the service.
// Depends on cc-provision-monitoring to limit concurrent SSM sessions (see
// nodes-nfs-mount comment).
resource "null_resource" "cc-provision-rpc-proxy" {
  depends_on = [
    null_resource.cc-ready,
    local_file.rpc_proxy_conf,
    local_file.rpc_proxy_compose,
    null_resource.cc-provision-monitoring,
  ]

  # Upload rpc-proxy config files
  provisioner "local-exec" {
    command = "scp -r ${local.ssh_opts} ${local.testnet_path}/rpc-proxy ${local.username}@${aws_instance.cc.id}:/home/${local.username}/"
  }

  # Start RPC proxy service
  provisioner "local-exec" {
    command = "ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} \"docker compose -f /home/${local.username}/rpc-proxy/compose.yaml up -d --quiet-pull\""
  }
}

// Provision pprof proxy config files and start the service.
resource "null_resource" "cc-provision-pprof-proxy" {
  depends_on = [
    null_resource.cc-ready,
    local_file.pprof_proxy_conf,
    local_file.pprof_proxy_compose,
  ]

  # Upload pprof-proxy config files
  provisioner "local-exec" {
    command = "scp -r ${local.ssh_opts} ${local.testnet_path}/pprof-proxy ${local.username}@${aws_instance.cc.id}:/home/${local.username}/"
  }

  # Start pprof proxy service
  provisioner "local-exec" {
    command = "ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} \"docker compose -f /home/${local.username}/pprof-proxy/compose.yaml up -d --quiet-pull\""
  }
}

// Provision monitoring config files and start monitoring services.
resource "null_resource" "cc-provision-monitoring" {
  depends_on = [
    null_resource.cc-ready,
    local_file.prometheus_yml,
    local_file.monitoring_compose,
  ]

  # Upload the entire local deployments/monitoring directory (static config files)
  provisioner "local-exec" {
    command = "scp -r ${local.ssh_opts} ${local.deployments_dir}/monitoring ${local.username}@${aws_instance.cc.id}:/home/${local.username}/"
  }

  # Upload compose.yaml for monitoring
  provisioner "local-exec" {
    command = "scp ${local.ssh_opts} ${local.testnet_path}/monitoring/compose.yaml ${local.username}@${aws_instance.cc.id}:/home/${local.username}/monitoring/compose.yaml"
  }

  # Overwrite prometheus.yml with local file generated from template
  provisioner "local-exec" {
    command = "scp ${local.ssh_opts} ${local.prometheus_yml_local_path} ${local.username}@${aws_instance.cc.id}:/home/${local.username}/monitoring/config-prometheus/prometheus.yml"
  }

  # Start monitoring services
  provisioner "local-exec" {
    command = "ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} \"docker compose -f /home/${local.username}/monitoring/compose.yaml up -d --quiet-pull\""
  }
}

# Locally build Spammer Docker image, while CC is being created
resource "terraform_data" "spammer-image" {
  # Build Docker image and save it to a tar.gz
  provisioner "local-exec" {
    command = <<-EOT
    docker build -q \
      --build-arg CIRCLE_BASE_IMAGE=${var.circle_base_image} \
      --platform linux/amd64 \
      -f crates/spammer/Dockerfile \
      -t spammer:latest . && \
    docker save spammer:latest | gzip > /tmp/spammer_latest.tar.gz
    EOT
    working_dir = local.root_dir
  }
}

# Upload Spammer Docker image once CC is ready
resource "terraform_data" "spammer-image-upload" {
  depends_on = [
    terraform_data.spammer-image,
    null_resource.cc-ready,
    local_file.infra_data,
  ]

  # Ensure remote directory exists (cloud-init may not have finished) and upload infra data file
  provisioner "local-exec" {
    command = <<-EOT
    ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} \
      'mkdir -p /home/${local.username}/${dirname(local.infra_data_remote_path)}' && \
    scp ${local.ssh_opts} ${local.infra_data_local_path} \
      ${local.username}@${aws_instance.cc.id}:/home/${local.username}/${local.infra_data_remote_path}
    EOT
  }

  # Upload tgz file of Spammer image to CC
  provisioner "local-exec" {
    command = "scp ${local.ssh_opts} /tmp/spammer_latest.tar.gz ${local.username}@${aws_instance.cc.id}:/home/${local.username}/spammer_latest.tar.gz"
  }

  # Load tgz file as Spammer Docker image in CC
  provisioner "local-exec" {
    command = "ssh ${local.ssh_opts} ${local.username}@${aws_instance.cc.id} \"docker load < /home/${local.username}/spammer_latest.tar.gz\""
  }
}
