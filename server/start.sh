#!/usr/bin/env bash
# start.sh — manage the Youn Ink server
#
# Usage:
#   ./start.sh start|stop|restart|status|logs
#
# Runs llmserve.py (WS + HTTP + UDP discovery) as a background process.
# For production, prefer systemd — see DEPLOY.md.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

PID_FILE="$HERE/data/server.pid"
LOG_FILE="$HERE/data/server.log"
VENV="$HERE/.venv"
PYTHON="$VENV/bin/python"

if [[ ! -x "$PYTHON" ]]; then
    echo "[ERROR] venv not found at $VENV — run:"
    echo "  python3 -m venv .venv && .venv/bin/pip install -r requirements.txt"
    exit 1
fi

is_running() {
    [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null
}

case "${1:-}" in
    start)
        if is_running; then
            echo "[INFO] already running (pid $(cat "$PID_FILE"))"
            exit 0
        fi
        echo "[INFO] starting llmserve.py..."
        nohup "$PYTHON" llmserve.py >> "$LOG_FILE" 2>&1 &
        PID=$!
        echo "$PID" > "$PID_FILE"
        sleep 2
        if ! kill -0 "$PID" 2>/dev/null; then
            echo "[ERROR] process exited immediately; tail of log:"
            tail -30 "$LOG_FILE"
            rm -f "$PID_FILE"
            exit 1
        fi
        echo "[OK] started pid=$PID  log=$LOG_FILE"
        ;;

    stop)
        if ! is_running; then
            echo "[INFO] not running"
            rm -f "$PID_FILE"
            exit 0
        fi
        PID=$(cat "$PID_FILE")
        echo "[INFO] stopping pid=$PID..."
        kill "$PID"
        for _ in $(seq 1 20); do
            if ! kill -0 "$PID" 2>/dev/null; then break; fi
            sleep 0.5
        done
        if kill -0 "$PID" 2>/dev/null; then
            echo "[WARN] did not exit gracefully; SIGKILL"
            kill -9 "$PID"
        fi
        rm -f "$PID_FILE"
        echo "[OK] stopped"
        ;;

    restart)
        "$0" stop
        "$0" start
        ;;

    status)
        if is_running; then
            echo "[OK] running pid=$(cat "$PID_FILE")"
            exit 0
        else
            echo "[INFO] not running"
            exit 1
        fi
        ;;

    logs)
        tail -f "$LOG_FILE"
        ;;

    *)
        echo "Usage: $0 {start|stop|restart|status|logs}"
        exit 2
        ;;
esac
