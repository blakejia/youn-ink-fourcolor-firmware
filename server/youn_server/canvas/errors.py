"""渲染器对外抛出的错误类型（单独一个文件，便于上层只依赖它）。"""
from __future__ import annotations


class RenderError(Exception):
    """元素/属性不支持或写错时抛出，带 JSON-pointer 风格路径便于定位。"""

    def __init__(self, path: str, message: str) -> None:
        super().__init__(f"{path}: {message}")
        self.path = path
        self.message = message
