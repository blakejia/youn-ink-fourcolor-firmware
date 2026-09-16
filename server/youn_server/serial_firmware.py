"""串口控制台用的固件仓库（与 OTA 频道严格分离）。

存储布局（settings.serial_firmware_dir）：

    serial-firmware/
        <ts>-<safe>.bin
        <ts>-<safe>.bin.sha256

只读来源（构建产物，不复制）：

    <repo>/firmware/build/xiaozhi.bin

约束来源见 spec：上传件按不可信处理；本模块**绝不**写 settings.firmware_dir。
"""
from __future__ import annotations

import hashlib
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

from .config import settings

#: ESP 应用镜像头首字节（其余 8 字节头字段不校验：浏览器侧只做粗筛，
#: 真正的芯片/镜像校验由 esptool-js 在设备上完成）
IMAGE_MAGIC = 0xE9
#: 应用分区实际大小（自 firmware/build/partition_table/partition-table.bin 解析）
MAX_IMAGE_BYTES = 0x3F0000
#: 仓库根（app.py 在 create_app 内直接 import 本模块）
_REPO_ROOT = Path(__file__).resolve().parents[2]
BUILD_ARTIFACT = _REPO_ROOT / "firmware" / "build" / "xiaozhi.bin"
BUILD_ID = "build:xiaozhi.bin"


@dataclass
class FirmwareItem:
    id: str
    name: str
    source: str  # "build" | "upload"
    size: int
    mtime: float
    sha256: str
    image_ok: bool

    def to_json(self) -> dict:
        return asdict(self)


def _sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _first_byte_is_magic(p: Path) -> bool:
    with p.open("rb") as f:
        return f.read(1) == bytes([IMAGE_MAGIC])


def safe_filename(name: str) -> str:
    """与 ota._safe_filename 同构：只保留字母数字与 . _ -"""
    return "".join(c for c in name if c.isalnum() or c in "._-").strip() or "firmware"


def validate_image(data: bytes) -> None:
    """不可信输入的唯一校验入口；失败抛 ValueError（中文原因，直接回给前端）。"""
    if not data:
        raise ValueError("空文件")
    if data[0] != IMAGE_MAGIC:
        raise ValueError("这不是 ESP 应用镜像（首字节应为 0xE9）")
    if len(data) > MAX_IMAGE_BYTES:
        raise ValueError(
            f"镜像过大：{len(data)} 字节，应用分区上限 {MAX_IMAGE_BYTES} 字节"
        )


def _item_from_path(p: Path, item_id: str, source: str) -> FirmwareItem:
    st = p.stat()
    return FirmwareItem(
        id=item_id,
        name=p.name,
        source=source,
        size=st.st_size,
        mtime=st.st_mtime,
        sha256=_sha256_file(p),
        image_ok=_first_byte_is_magic(p),
    )


def list_items() -> list[FirmwareItem]:
    items: list[FirmwareItem] = []
    if BUILD_ARTIFACT.is_file():
        items.append(_item_from_path(BUILD_ARTIFACT, BUILD_ID, "build"))
    d = settings.serial_firmware_dir
    if d.is_dir():
        for p in sorted(d.glob("*.bin")):
            items.append(_item_from_path(p, f"upload:{p.name}", "upload"))
    return items


def resolve_item(item_id: str) -> Optional[Path]:
    """把不透明 id 解析成白名单内的真实路径；解析不出返回 None。

    ``safe_filename`` 已经剥掉 ``/`` 与 ``.`` 之外的字符，因此遍历串在
    拼接前就被破坏；解析后再做一次父目录校验，双重保险（照 ota.get_file_path）。
    """
    if item_id == BUILD_ID:
        return BUILD_ARTIFACT if BUILD_ARTIFACT.is_file() else None
    if item_id.startswith("upload:"):
        name = safe_filename(item_id[len("upload:"):])
        p = settings.serial_firmware_dir / name
        if not p.is_file():
            return None
        if settings.serial_firmware_dir.resolve() not in p.resolve().parents:
            return None
        return p
    return None


def save_upload(filename: str, data: bytes) -> FirmwareItem:
    validate_image(data)
    d = settings.serial_firmware_dir
    d.mkdir(parents=True, exist_ok=True)
    stem = Path(safe_filename(filename)).stem or "firmware"
    name = f"{int(time.time())}-{stem}.bin"
    p = d / name
    p.write_bytes(data)
    sha = hashlib.sha256(data).hexdigest()
    (d / f"{name}.sha256").write_text(sha + "\n")
    return FirmwareItem(
        id=f"upload:{name}",
        name=name,
        source="upload",
        size=len(data),
        mtime=p.stat().st_mtime,
        sha256=sha,
        image_ok=True,
    )
