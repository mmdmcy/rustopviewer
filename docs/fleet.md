# Fleet mode

Last updated: 2026-08-02

## Model

- One always-on machine runs `rustopviewer host` (registry + local desktop).
- Other machines run `rustopviewer agent --host http://<registry-tailscale-ip>:45080`.
- Every device shares the same `DALE_TOKEN` / `ROV_MASTERDALE_TOKEN`.

## Registry host

```bash
cargo install --path . --root "$HOME/.local"
mkdir -p ~/.config/rustopviewer
cat > ~/.config/rustopviewer/masterdale.env <<'EOF'
DALE_TOKEN=<same-as-masterdale>
ROV_DEVICE_CODE=REGISTRY
EOF
cp packaging/systemd/rustopviewer-host.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now rustopviewer-host.service
```

Open `http://<registry-tailscale-ip>:45080/`, choose **Use Masterdale Token**, then pick a registered device from the fleet list.

## Agent devices

```bash
cargo install --path . --root "$HOME/.local"
mkdir -p ~/.config/rustopviewer
cat > ~/.config/rustopviewer/masterdale.env <<'EOF'
DALE_TOKEN=<same-as-masterdale>
ROV_FLEET_HOST=http://<registry-tailscale-ip>:45080
ROV_DEVICE_CODE=WORKSTATION
EOF
cp packaging/systemd/rustopviewer-agent.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now rustopviewer-agent.service
```

## CLI checks

```bash
rustopviewer status
rustopviewer devices --host http://<registry-tailscale-ip>:45080
rustopviewer open workstation --host http://<registry-tailscale-ip>:45080
```

## Notes

- Browser localStorage does not cross agent origins. Opening an agent from the host dashboard appends `#masterdale=...` once so the agent page can store the token.
- macOS capture depends on `xcap` permissions; the agent still registers even if capture is unavailable.
- Admin routes stay loopback + `ROV_ADMIN_TOKEN`. Masterdale bearer must not unlock admin.
