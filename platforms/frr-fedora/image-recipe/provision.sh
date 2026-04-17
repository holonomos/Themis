#!/usr/bin/env bash
set -euo pipefail

# Versions mirrored from platforms/frr-fedora/platform.yml — keep in sync.
NODE_EXPORTER_VER="1.7.0"
FRR_EXPORTER_VER="1.10.1"
PROMTAIL_VER="2.9.3"

echo "==> [1/8] Installing packages"
# cloud-init + cloud-utils: consumes the seed ISO attached to seed-mode VMs
# at first boot (hostname + network-config). cloud-utils provides
# cloud-localds which the ansible vm-bootstrap role uses on the host to
# build seed ISOs — it's harmless but handy to ship in the guest image too.
dnf install -y frr chrony rsyslog dnsmasq iptables-services bridge-utils iproute bind-utils tcpdump nftables tar curl ca-certificates unzip cloud-init cloud-utils

echo "==> [2/8] Installing Prom exporters and Promtail"
curl -sL "https://github.com/prometheus/node_exporter/releases/download/v${NODE_EXPORTER_VER}/node_exporter-${NODE_EXPORTER_VER}.linux-amd64.tar.gz" | tar -xz -C /tmp/
mv -f "/tmp/node_exporter-${NODE_EXPORTER_VER}.linux-amd64/node_exporter" /usr/local/bin/

curl -sL "https://github.com/tynany/frr_exporter/releases/download/v${FRR_EXPORTER_VER}/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64.tar.gz" | tar -xz -C /tmp/
mv -f "/tmp/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64/frr_exporter" /usr/local/bin/

curl -sLo /tmp/promtail.zip "https://github.com/grafana/loki/releases/download/v${PROMTAIL_VER}/promtail-linux-amd64.zip"
unzip -q -o /tmp/promtail.zip -d /tmp/
mv -f /tmp/promtail-linux-amd64 /usr/local/bin/promtail
rm -f /tmp/promtail.zip

chown root:root /usr/local/bin/node_exporter /usr/local/bin/frr_exporter /usr/local/bin/promtail
chmod 0755 /usr/local/bin/node_exporter /usr/local/bin/frr_exporter /usr/local/bin/promtail
rm -rf "/tmp/node_exporter-${NODE_EXPORTER_VER}.linux-amd64" "/tmp/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64"

echo "==> [3/8] Installing systemd units"

cat << 'EOF' > /etc/systemd/system/node_exporter.service
[Unit]
Description=Prometheus Node Exporter
After=network-online.target

[Service]
ExecStart=/usr/local/bin/node_exporter --web.listen-address=:9100
Restart=on-failure
DynamicUser=true

[Install]
WantedBy=multi-user.target
EOF

cat << 'EOF' > /etc/systemd/system/frr_exporter.service
[Unit]
Description=FRR Exporter
After=network-online.target frr.service

[Service]
ExecStart=/usr/local/bin/frr_exporter --web.listen-address=:9342
Restart=on-failure
User=root

[Install]
WantedBy=multi-user.target
EOF

cat << 'EOF' > /etc/systemd/system/promtail.service
[Unit]
Description=Promtail Log Shipper
After=network-online.target

[Service]
ExecStart=/usr/local/bin/promtail -config.file=/etc/promtail/config.yml
Restart=on-failure
User=root

[Install]
WantedBy=multi-user.target
EOF

echo "==> [4/8] Sysctl configuration"
cat << 'EOF' > /etc/sysctl.d/99-themis.conf
net.ipv4.ip_forward=1
net.ipv6.conf.all.forwarding=1
net.ipv4.conf.all.rp_filter=0
net.ipv4.conf.default.rp_filter=0
EOF

echo "==> [5/8] SELinux"
sed -i 's/^SELINUX=enforcing/SELINUX=permissive/' /etc/selinux/config
setenforce 0 || true

echo "==> [6/8] Enable at boot"
systemctl enable chronyd rsyslog sshd frr
# cloud-init stays enabled so the seed ISO is honored at first boot;
# dhcp-mode VMs get their lease via NetworkManager's cloud-init fallthrough
# (cloud-init finds no seed, drops into NoCloud-network which DHCPs normally).
systemctl enable cloud-init cloud-config cloud-final cloud-init-local

echo "==> [7/8] Clean for cloning"
> /etc/machine-id
rm -f /etc/ssh/ssh_host_*
dnf clean all
journalctl --vacuum-time=1s || true
truncate -s 0 /var/log/lastlog /var/log/wtmp /var/log/btmp || true

echo "==> [8/8] Compact the image"
dd if=/dev/zero of=/var/tmp/zeroes bs=1M || true
rm -f /var/tmp/zeroes
