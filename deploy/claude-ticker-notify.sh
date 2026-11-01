#!/bin/sh
# Tell the claude-ticker display that Claude Code needs you (or doesn't any
# more). Run on your computer, from a Claude Code hook; see README.md.
#   claude-ticker-notify attention    show the "i" icon
#   claude-ticker-notify clear        remove it
# Reads the Pi's address and token from ~/.config/claude-ticker/notify.env:
#   CLAUDE_TICKER_URL=http://claude-ticker.local:8090
#   CLAUDE_TICKER_TOKEN=the same token as [notify] token on the Pi
# Always exits 0 and stays quiet, so a Pi that is off never gets in the way.
CONF="${CLAUDE_TICKER_NOTIFY:-${XDG_CONFIG_HOME:-$HOME/.config}/claude-ticker/notify.env}"
case "${1:-}" in attention | clear) ;; *) exit 0 ;; esac
[ -r "$CONF" ] || exit 0
# shellcheck source=/dev/null
. "$CONF"
[ -n "${CLAUDE_TICKER_URL:-}" ] && [ -n "${CLAUDE_TICKER_TOKEN:-}" ] || exit 0
# The header goes through stdin so the token never shows up in `ps`.
printf 'header = "Authorization: Bearer %s"\n' "$CLAUDE_TICKER_TOKEN" |
    curl -fsS -m 3 -o /dev/null -X POST -K - "$CLAUDE_TICKER_URL/$1" 2>/dev/null
exit 0
