# File with info on the created infrastructure, used by Quake in remote mode.
# Must depend on secondary ENI attachments so that network_ips includes all
# interfaces for bridge nodes.
resource "local_file" "infra_data" {
  depends_on = [
    aws_instance.node,
    aws_instance.cc,
    aws_network_interface.node_secondary_eni,
    aws_network_interface_attachment.node_secondary_eni_attachment,
  ]
  content = templatefile("${path.module}/templates/infra-data-json.tmpl", {
    project_name = local.project_name,
    cc           = local.cc,
    nodes        = local.nodes
  })
  filename = local.infra_data_local_path
}

output "cc_instance_url" {
  value = "https://console.aws.amazon.com/ec2/home#InstanceDetails:instanceId=${aws_instance.cc.id}"
}

output "nodes_instance_urls" {
  value = {
    for node in aws_instance.node : node.id => "https://console.aws.amazon.com/ec2/home#InstanceDetails:instanceId=${node.id}"
  }
}
