"""超时辅助。"""
from __future__ import annotations

import asyncio
from collections.abc import Awaitable
from typing import TypeVar

T = TypeVar("T")


async def with_timeout(coro: Awaitable[T], timeout_s: float) -> T:
    """包装 awaitable，超时抛 asyncio.TimeoutError。"""
    return await asyncio.wait_for(coro, timeout=timeout_s)
