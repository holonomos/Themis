#!/usr/bin/env bash
set -ex

# Install FRR and necessary tools from Fedora repos
dnf install -y frr chrony rsyslog dnsmasq iptables-services

# We assume frr_exporter, node_exporter, and promtail binary installations are 
# handled here or by a base vagrant box. For greenfield, they're omitted if not strictly available via DNF.
# We'll just install standard packages.

systemctl enable frr chronyd rsyslog

cat <<EOF > /etc/sysctl.d/99-routing.conf
net.ipv4.ip_forward=1
EOF

# Clean cache to reduce image size
dnf clean all
dd if=/dev/zero of=/var/tmp/zeroes bs=1M || true
rm -f /var/tmp/zeroes
