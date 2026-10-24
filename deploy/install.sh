#!/bin/sh
# Install claude-ticker as a boot-time service. Run on the Pi:
#   sudo ./install.sh ./claude-ticker [credentials.json]
# Safe to re-run: upgrades the binary and unit, never overwrites your config
# or sign-in. Sign in afterwards with `sudo claude-ticker login`, or pass a
# credentials file exported from a Mac (see README.md) to install that.
set -eu

BIN="${1:-./claude-ticker}"
CREDS="${2:-}"
HERE="$(cd "$(dirname "$0")" && pwd)"
STATE=/var/lib/claude-ticker

if [ "$(id -u)" -ne 0 ]; then
    echo "run with sudo: sudo $0 $BIN $CREDS" >&2
    exit 1
fi
if [ ! -f "$BIN" ]; then
    echo "binary not found: $BIN (build it with cross, see README.md)" >&2
    exit 1
fi

install -m 0755 "$BIN" /usr/local/bin/claude-ticker
install -m 0644 "$HERE/claude-ticker.service" /etc/systemd/system/claude-ticker.service

install -d -m 0755 /etc/claude-ticker
if [ ! -f /etc/claude-ticker/config.toml ]; then
    install -m 0644 "$HERE/config.toml" /etc/claude-ticker/config.toml
    echo "created /etc/claude-ticker/config.toml (set your timezone there)"
fi

if [ -n "$CREDS" ]; then
    systemctl stop claude-ticker.service 2>/dev/null || true
    # With DynamicUser, systemd keeps the real directory under
    # /var/lib/private once the service has run. Match its owner so the
    # service can read the file; on a first install systemd chowns it at start.
    if [ -d /var/lib/private/claude-ticker ]; then
        DIR=/var/lib/private/claude-ticker
        OWNER="$(stat -c %u "$DIR"):$(stat -c %g "$DIR")"
    else
        DIR=$STATE
        OWNER=0:0
        install -d -m 0700 "$DIR"
    fi
    install -m 0600 -o "${OWNER%:*}" -g "${OWNER#*:}" "$CREDS" "$DIR/credentials.json"
    echo "installed credentials; now run /login in Claude Code on your Mac so the"
    echo "Mac and the Pi stop sharing one sign-in (see README.md)"
elif [ ! -f /var/lib/private/claude-ticker/credentials.json ] && [ ! -f $STATE/credentials.json ]; then
    echo "not signed in yet: scan the QR code on the display, or run: sudo claude-ticker login"
fi

systemctl daemon-reload
# The app waits for NTP itself; make sure timesyncd (which it watches) is on.
systemctl enable --now systemd-timesyncd.service

# The HAT talks over SPI, which Raspberry Pi OS leaves off by default. Turn it
# on: uncomment or flip an existing dtparam=spi line, or add one under [all]
# (the file may end inside a model-specific section such as [cm4]).
BOOTCFG=/boot/firmware/config.txt
[ -f "$BOOTCFG" ] || BOOTCFG=/boot/config.txt    # Raspberry Pi OS before Bookworm
REBOOT=""
if [ ! -f "$BOOTCFG" ]; then
    echo "warning: no config.txt found; add dtparam=spi=on to it by hand and reboot" >&2
elif ! grep -Eq '^[[:space:]]*dtparam=([^#]*,)?spi(=on)?([,[:space:]#]|$)' "$BOOTCFG"; then
    cp "$BOOTCFG" "$BOOTCFG.claude-ticker.bak"
    if grep -Eq '^[[:space:]]*#?[[:space:]]*dtparam=([^#]*,)?spi=' "$BOOTCFG"; then
        sed -E -i 's/^[[:space:]]*#?[[:space:]]*(dtparam=([^#]*,)?spi=)[a-z]+/\1on/' "$BOOTCFG"
    else
        printf '\n[all]\ndtparam=spi=on\n' >> "$BOOTCFG"
    fi
    REBOOT=1
    echo "enabled SPI in $BOOTCFG (backup: $BOOTCFG.claude-ticker.bak)"
fi

systemctl enable claude-ticker.service
systemctl restart claude-ticker.service
echo "installed; status: systemctl status claude-ticker   logs: journalctl -u claude-ticker -f"
if [ -n "$REBOOT" ]; then
    echo "SPI was just enabled; reboot for the display to work: sudo reboot"
fi
