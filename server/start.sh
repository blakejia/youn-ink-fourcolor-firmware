#!/usr/bin/env bash
# start.sh — control the Youn Ink server through its systemd --user unit.
#
# Usage:
#   ./start.sh install | uninstall
#   ./start.sh start | stop | restart | status
#   ./start.sh logs | journal
#
# The unit is versioned at systemd/youn-ink-server.service and symlinked into
# ~/.config/systemd/user/ by `install`, so the repo holds the single source of
# truth. This host has no root, so the service lives in the user manager; with
# lingering enabled that still means start-at-boot and survival after logout.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT="youn-ink-server.service"
UNIT_SRC="$HERE/systemd/$UNIT"
UNIT_DST="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$UNIT"
LOG_FILE="$HERE/data/server.log"

usage() {
    cat <<'EOF'
Usage: ./start.sh {install|uninstall|start|stop|restart|status|logs|journal}

  install     symlink the unit into the user manager, enable it, enable lingering
  uninstall   stop, disable and remove the unit
  start       start the service
  stop        stop the service
  restart     restart the service
  status      show unit status (exit code follows systemd)
  logs        follow the application log (data/server.log, rotating)
  journal     follow stdout/stderr (uvicorn access lines, tracebacks)
EOF
}

require_installed() {
    if [[ ! -e "$UNIT_DST" ]]; then
        echo "[ERROR] $UNIT is not installed — run: ./start.sh install" >&2
        exit 1
    fi
}

case "${1:-}" in
    install)
        if [[ ! -x "$HERE/.venv/bin/python" ]]; then
            echo "[ERROR] venv not found at $HERE/.venv — run:"
            echo "  python3 -m venv .venv && .venv/bin/pip install -r requirements.txt"
            exit 1
        fi
        mkdir -p "$(dirname "$UNIT_DST")"
        ln -sfn "$UNIT_SRC" "$UNIT_DST"
        systemctl --user daemon-reload
        systemctl --user enable "$UNIT"
        # Without lingering the user manager (and the service) dies with the last
        # session. Needs no root for your own user.
        if loginctl enable-linger "$(id -un)" 2>/dev/null; then
            echo "[OK] lingering enabled for $(id -un)"
        else
            echo "[WARN] could not enable lingering; the service will stop at logout."
            echo "       run as root: loginctl enable-linger $(id -un)"
        fi
        echo "[OK] installed: $UNIT_DST -> $UNIT_SRC"
        echo "     start it with: ./start.sh start"
        ;;

    uninstall)
        systemctl --user disable --now "$UNIT" 2>/dev/null || true
        rm -f "$UNIT_DST"
        systemctl --user daemon-reload
        echo "[OK] removed $UNIT_DST"
        ;;

    start)
        require_installed
        systemctl --user start "$UNIT"
        systemctl --user --no-pager --lines=0 status "$UNIT" | head -10
        ;;

    stop)
        require_installed
        systemctl --user stop "$UNIT"
        echo "[OK] stopped"
        ;;

    restart)
        require_installed
        systemctl --user restart "$UNIT"
        systemctl --user --no-pager --lines=0 status "$UNIT" | head -10
        ;;

    status)
        require_installed
        systemctl --user --no-pager status "$UNIT"
        ;;

    logs)
        # The application's own rotating file (RotatingFileHandler, 10 MB x 5).
        tail -f "$LOG_FILE"
        ;;

    journal)
        # Everything the process wrote to stdout/stderr.
        journalctl --user -u "$UNIT" -f -n 50
        ;;

    *)
        usage
        exit 2
        ;;
esac
