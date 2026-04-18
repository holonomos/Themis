#!/usr/bin/env bash
set -euo pipefail

# Versions mirrored from platforms/cumulus-vx/platform.yml — keep in sync.
NODE_EXPORTER_VER="1.7.0"
FRR_EXPORTER_VER="1.10.1"
PROMTAIL_VER="2.9.3"

echo "==> [1/7] Installing observability binaries"
# Cumulus Linux is Debian 11. FRR and NVUE are already installed.
apt-get update
apt-get install -y --no-install-recommends curl unzip tar ca-certificates

curl -sL "https://github.com/prometheus/node_exporter/releases/download/v${NODE_EXPORTER_VER}/node_exporter-${NODE_EXPORTER_VER}.linux-amd64.tar.gz" | tar -xz -C /tmp/
install -m0755 "/tmp/node_exporter-${NODE_EXPORTER_VER}.linux-amd64/node_exporter" /usr/local/bin/node_exporter

curl -sL "https://github.com/tynany/frr_exporter/releases/download/v${FRR_EXPORTER_VER}/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64.tar.gz" | tar -xz -C /tmp/
install -m0755 "/tmp/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64/frr_exporter" /usr/local/bin/frr_exporter

curl -sLo /tmp/promtail.zip "https://github.com/grafana/loki/releases/download/v${PROMTAIL_VER}/promtail-linux-amd64.zip"
unzip -q -o /tmp/promtail.zip -d /tmp/
install -m0755 /tmp/promtail-linux-amd64 /usr/local/bin/promtail

rm -rf "/tmp/node_exporter-${NODE_EXPORTER_VER}.linux-amd64" "/tmp/frr_exporter-${FRR_EXPORTER_VER}.linux-amd64" /tmp/promtail.zip /tmp/promtail-linux-amd64

echo "==> [2/7] Installing systemd units"

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

systemctl daemon-reload

echo "==> [3/7] Sysctl"
cat << 'EOF' > /etc/sysctl.d/99-themis.conf
net.ipv4.ip_forward=1
net.ipv6.conf.all.forwarding=1
net.ipv4.conf.all.rp_filter=0
net.ipv4.conf.default.rp_filter=0
EOF
sysctl --system

echo "==> [4/7] Ensure NVUE + FRR enabled at boot"
# nvued and frr are present on Cumulus 5.9 but confirm enabled.
systemctl enable nvued frr ssh

echo "==> [5/7] Create NVUE startup config directory"
# Themis writes its generated config to /etc/nvue.d/startup.yaml. Ensure parent dir exists.
install -d -m0755 /etc/nvue.d

echo "==> [6/7] Clean for cloning"
> /etc/machine-id
rm -f /etc/ssh/ssh_host_*
apt-get clean
rm -rf /var/lib/apt/lists/*
journalctl --vacuum-time=1s || true
truncate -s 0 /var/log/lastlog /var/log/wtmp /var/log/btmp || true

echo "==> [7/7] Compact the image"
dd if=/dev/zero of=/var/tmp/zeroes bs=1M || true
rm -f /var/tmp/zeroes
