#!/usr/bin/env bash
# Builds Flow and installs it as a systemd user service.
#
# This does not touch ~/.config/hypr/bindings.lua and does not enable or
# start the service: those are one-time, machine-specific steps you do
# yourself once you have checked the bind lines this script prints do not
# collide with anything else already bound to Right Ctrl.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
bin_dir="$HOME/.local/bin"
unit_dir="$config_home/systemd/user"

echo "Building flow-core (release) ..."
(cd "$repo_root" && cargo build --release)

mkdir -p "$bin_dir"
install -m 755 "$repo_root/target/release/flow-core" "$bin_dir/flow-core"
echo "Installed $bin_dir/flow-core"

mkdir -p "$unit_dir"
unit_file="$unit_dir/flow.service"
cat > "$unit_file" <<EOF
[Unit]
Description=Flow: push-to-talk local dictation

[Service]
Type=simple
ExecStart=%h/.local/bin/flow-core
Restart=on-failure
Environment=FLOW_ECHO=0

[Install]
WantedBy=graphical-session.target
EOF
echo "Wrote $unit_file"

systemctl --user daemon-reload
echo "Ran systemctl --user daemon-reload"

cat <<'EOF'

Not done yet: add these two lines to ~/.config/hypr/bindings.lua, replacing
any existing Right Ctrl binding (voxtype's, if that is what is bound there):

  o.bind("CONTROL_R", "Flow: hold to dictate", "flow-core --key down")
  o.bind("CTRL + CONTROL_R", "Flow: release", "flow-core --key up", { release = true })

Then reload Hyprland's config and start the service yourself:

  systemctl --user enable --now flow.service
EOF
