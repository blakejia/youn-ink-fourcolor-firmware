#!/usr/bin/env bash
# 防砖流程第 1 步：完整备份原厂 flash（16MB）
# 用法: ./brick_safe_backup.sh [port]
# 产出: backup/flash-backup-<timestamp>.bin + SHA256
set -euo pipefail

PORT="${1:-/dev/ttyACM0}"
BACKUP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/backup"
mkdir -p "$BACKUP_DIR"
TS=$(date +%Y%m%d-%H%M%S)
OUT="$BACKUP_DIR/flash-backup-$TS.bin"

if [[ ! -e "$PORT" ]]; then
    echo "[ERROR] 串口不存在: $PORT"
    echo "设备可能: ① 深度睡眠中（按 BOOT 唤醒）② 需进下载模式（按住 BOOT 点 RESET）"
    exit 1
fi

echo "[1/3] 校验芯片连接..."
. /home/pi/data/esp-idf-v6.0/export.sh > /dev/null 2>&1
python -m esptool --chip esp32s3 --port "$PORT" --baud 115200 chip-id

echo "[2/3] 备份 16MB flash（约 3-5 分钟，勿断线）..."
python -m esptool --chip esp32s3 --port "$PORT" --baud 460800 \
    read-flash 0x0 0x1000000 "$OUT"

echo "[3/3] 校验..."
SHA=$(sha256sum "$OUT" | cut -d' ' -f1)
echo "$SHA  $OUT" > "$OUT.sha256"
SIZE=$(stat -c%s "$OUT")
echo "[OK] 备份完成: $OUT ($SIZE bytes)"
echo "[OK] SHA-256: $SHA"
if [[ "$SIZE" -ne 16777216 ]]; then
    echo "[WARN] 备份大小异常（期望 16777216），刷写前必须排查！"
    exit 1
fi
echo "[OK] 防砖备份就绪。现在可以安全刷写。"
