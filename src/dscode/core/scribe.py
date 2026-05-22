"""Scribe —— 三级记忆引擎。

设计要点：
- L1 RawEvent：无条件写入，永不丢失（事件溯源）。
- L2 Fact：写入闸门 —— `provenance_chain` 必须非空（至少一条工具调用 RawEvent ID）。
- L3 Pattern：v1 直接写入，稳定性检测留 Anvil v2 实现。
- SQLite + WAL 模式，单连接 + asyncio.to_thread 包装同步调用（零依赖）。
- 文件镜像 `.dscode/memory/raw/<session_id>.jsonl`，人类可审计 / 可 grep。
- FTS5 全文索引，CJK 用 unicode61 + remove_diacritics 0；trigram 留 TODO 优化。
"""
from __future__ import annotations

import asyncio
import json
import sqlite3
import threading
from pathlib import Path
from typing import Any

from dscode.core.types import (
    ContextPacket,
    Fact,
    FactWriteResult,
    Pattern,
    PatternWriteResult,
    RawEvent,
)

# ============================================================
# SQL Schema
# ============================================================

_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS raw_events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    task_id TEXT,
    timestamp REAL NOT NULL,
    step_number INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    data JSON NOT NULL,
    created_at REAL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_raw_session ON raw_events(session_id, step_number);
CREATE INDEX IF NOT EXISTS idx_raw_task ON raw_events(task_id);

CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    object TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 1.0,
    provenance_chain JSON NOT NULL,
    source_raw_event_id TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    created_at REAL DEFAULT (unixepoch()),
    updated_at REAL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_facts_status ON facts(status);

CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    subject, predicate, object,
    tokenize='unicode61 remove_diacritics 0'
);
-- TODO(scribe): 引入 trigram tokenizer 改善中文短词召回；目前 unicode61 已可用。

CREATE TABLE IF NOT EXISTS patterns (
    id TEXT PRIMARY KEY,
    pattern_type TEXT NOT NULL,
    trigger_condition TEXT NOT NULL,
    action_template JSON NOT NULL,
    success_rate REAL NOT NULL DEFAULT 0.0,
    sample_count INTEGER NOT NULL DEFAULT 0,
    confidence REAL NOT NULL DEFAULT 0.0,
    last_triggered_at REAL,
    session_ids JSON NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at REAL DEFAULT (unixepoch()),
    updated_at REAL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_patterns_status ON patterns(status);
CREATE INDEX IF NOT EXISTS idx_patterns_confidence ON patterns(confidence);

CREATE VIRTUAL TABLE IF NOT EXISTS patterns_fts USING fts5(
    trigger_condition,
    tokenize='unicode61 remove_diacritics 0'
);
"""


# ============================================================
# Helpers：粗略 token 估计
# ============================================================

def _estimate_tokens(text: str) -> int:
    """非常粗的 token 估计：4 字节 ≈ 1 token（OpenAI 经验值）。

    用于 ContextPacket 预算控制；准确计数留给 LLM provider。
    """
    return max(1, len(text.encode("utf-8")) // 4)


# ============================================================
# Scribe
# ============================================================

class Scribe:
    """三级记忆引擎。

    Args:
        db_path: SQLite 数据库路径，默认 `.dscode/memory/state.db`。
        mirror_dir: 文件镜像目录，默认 db_path 的 raw/ 子目录。

    线程安全：内部用一把 lock 保护连接（sqlite3 单连接非线程安全）。
    所有公共方法均为 async，内部通过 `asyncio.to_thread` 调度同步操作。
    """

    def __init__(
        self,
        db_path: str | Path = ".dscode/memory/state.db",
        mirror_dir: str | Path | None = None,
    ) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)

        if mirror_dir is None:
            mirror_dir = self.db_path.parent / "raw"
        self.mirror_dir = Path(mirror_dir)
        self.mirror_dir.mkdir(parents=True, exist_ok=True)

        # 连接 + 锁
        self._lock = threading.Lock()
        self._conn = sqlite3.connect(
            str(self.db_path),
            check_same_thread=False,
            isolation_level=None,  # autocommit；我们手动管事务
        )
        self._conn.row_factory = sqlite3.Row
        self._init_db()

    # -------- 初始化 / 关闭 --------

    def _init_db(self) -> None:
        with self._lock:
            self._conn.execute("PRAGMA journal_mode=WAL")
            self._conn.execute("PRAGMA synchronous=NORMAL")
            self._conn.execute("PRAGMA foreign_keys=ON")
            self._conn.executescript(_SCHEMA_SQL)

    def close(self) -> None:
        """关闭数据库连接。"""
        with self._lock:
            self._conn.close()

    # -------- 写：RawEvent --------

    async def write_raw(self, event: RawEvent) -> None:
        """无条件写入 L1 原始事件 + 追加文件镜像。"""
        await asyncio.to_thread(self._write_raw_sync, event)

    def _write_raw_sync(self, event: RawEvent) -> None:
        payload = json.dumps(event.data, ensure_ascii=False, default=str)
        with self._lock:
            self._conn.execute(
                """
                INSERT OR REPLACE INTO raw_events
                  (id, session_id, task_id, timestamp, step_number, event_type, data)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    event.id,
                    event.session_id,
                    event.task_id,
                    event.timestamp,
                    event.step_number,
                    event.event_type,
                    payload,
                ),
            )

        # 文件镜像（追加 jsonl，按 session 分文件）
        mirror_path = self.mirror_dir / f"{event.session_id}.jsonl"
        line = event.model_dump_json() + "\n"
        # 文件写入也加锁保证多协程下顺序
        with self._lock:
            with mirror_path.open("a", encoding="utf-8") as f:
                f.write(line)

    # -------- 写：Fact（带闸门） --------

    async def write_fact(self, fact: Fact) -> FactWriteResult:
        """L2 写入闸门：provenance_chain 必须非空。"""
        if not fact.provenance_chain:
            return FactWriteResult(
                accepted=False,
                reason="missing provenance: fact.provenance_chain 不能为空",
            )
        return await asyncio.to_thread(self._write_fact_sync, fact)

    def _write_fact_sync(self, fact: Fact) -> FactWriteResult:
        provenance = json.dumps(fact.provenance_chain, ensure_ascii=False)
        with self._lock:
            self._conn.execute(
                """
                INSERT OR REPLACE INTO facts
                  (id, subject, predicate, object, confidence,
                   provenance_chain, source_raw_event_id, status,
                   created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    fact.id,
                    fact.subject,
                    fact.predicate,
                    fact.object,
                    fact.confidence,
                    provenance,
                    fact.source_raw_event_id,
                    fact.status,
                    fact.created_at,
                    fact.updated_at,
                ),
            )
            # FTS5 镜像：先删后插，保证幂等
            self._conn.execute("DELETE FROM facts_fts WHERE rowid = ?", (self._fts_rowid(fact.id),))
            cur = self._conn.execute(
                "INSERT INTO facts_fts(rowid, subject, predicate, object) VALUES (?, ?, ?, ?)",
                (
                    self._fts_rowid(fact.id),
                    fact.subject,
                    fact.predicate,
                    fact.object,
                ),
            )
            _ = cur
        return FactWriteResult(accepted=True, fact_id=fact.id)

    @staticmethod
    def _fts_rowid(uid: str) -> int:
        """fts5 rowid 必须是 int；用 id 的 hash 兜底（碰撞概率极低）。"""
        return int(uid[:8], 16) if all(c in "0123456789abcdef" for c in uid[:8]) else abs(hash(uid))

    # -------- 写：Pattern（L3 稳定性闸门） --------

    # 稳定性规则常量（Anvil 也读这些值）
    STABILITY_MIN_SESSIONS = 5
    STABILITY_MIN_SPAN_S = 24 * 3600       # 24 小时
    STABILITY_MAX_IDLE_S = 30 * 24 * 3600  # 30 天

    async def write_pattern(self, pattern: Pattern) -> PatternWriteResult:
        """L3 稳定性闸门。

        合并 + 升级策略：
        1. 同 (trigger_condition, pattern_type) 已存在 active/candidate
           → 合并：sample_count 累加、session_ids 取并集、success_rate 加权平均、
             last_triggered_at = max(...)，并视稳定性条件升级状态。
        2. 不存在 → 写入并默认 status='candidate'。
        3. 稳定性条件：
           - len(session_ids) >= STABILITY_MIN_SESSIONS（>=5）
           - max(last_triggered) - created_at >= 24h
           - now - last_triggered_at <= 30 天
           满足 → status='active'，否则保持 'candidate'。
        4. 调用方显式传 status='active' 但不满足稳定性 → 降级为 'candidate' 并在 reason 中说明。

        Returns:
            PatternWriteResult(accepted, pattern_id, reason)。
            合并时 pattern_id 返回**已存在**的那条 id（即使传入的 pattern.id 不同）。
        """
        return await asyncio.to_thread(self._write_pattern_sync, pattern)

    def _write_pattern_sync(self, pattern: Pattern) -> PatternWriteResult:
        with self._lock:
            row = self._conn.execute(
                """
                SELECT * FROM patterns
                WHERE trigger_condition = ? AND pattern_type = ?
                  AND status != 'retired'
                LIMIT 1
                """,
                (pattern.trigger_condition, pattern.pattern_type),
            ).fetchone()

        if row is not None:
            return self._merge_pattern_sync(row, pattern)
        return self._insert_new_pattern_sync(pattern)

    def _insert_new_pattern_sync(self, pattern: Pattern) -> PatternWriteResult:
        """写入新候选 Pattern。"""
        # 默认放到 candidate；只有满足稳定性条件才允许 active
        target_status = pattern.status
        if target_status == "active" and not _is_stable(
            session_ids=pattern.session_ids,
            created_at=pattern.created_at,
            last_triggered_at=pattern.last_triggered_at,
            min_sessions=self.STABILITY_MIN_SESSIONS,
            min_span_s=self.STABILITY_MIN_SPAN_S,
            max_idle_s=self.STABILITY_MAX_IDLE_S,
        ):
            target_status = "candidate"
        elif target_status not in ("candidate", "active", "retired"):
            target_status = "candidate"

        action = json.dumps(pattern.action_template, ensure_ascii=False, default=str)
        sessions = json.dumps(pattern.session_ids, ensure_ascii=False)
        with self._lock:
            self._conn.execute(
                """
                INSERT OR REPLACE INTO patterns
                  (id, pattern_type, trigger_condition, action_template,
                   success_rate, sample_count, confidence, last_triggered_at,
                   session_ids, status, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    pattern.id,
                    pattern.pattern_type,
                    pattern.trigger_condition,
                    action,
                    pattern.success_rate,
                    pattern.sample_count,
                    pattern.confidence,
                    pattern.last_triggered_at,
                    sessions,
                    target_status,
                    pattern.created_at,
                    pattern.updated_at,
                ),
            )
            self._conn.execute(
                "DELETE FROM patterns_fts WHERE rowid = ?",
                (self._fts_rowid(pattern.id),),
            )
            self._conn.execute(
                "INSERT INTO patterns_fts(rowid, trigger_condition) VALUES (?, ?)",
                (self._fts_rowid(pattern.id), pattern.trigger_condition),
            )
        reason = None
        if target_status != pattern.status and pattern.status == "active":
            reason = "稳定性条件未满足，已降级为 candidate"
        elif target_status == "candidate":
            reason = "新候选 pattern，等待稳定性升级"
        return PatternWriteResult(accepted=True, pattern_id=pattern.id, reason=reason)

    def _merge_pattern_sync(
        self, existing: sqlite3.Row, incoming: Pattern
    ) -> PatternWriteResult:
        """已有 active/candidate 同主键 → 合并样本，可能升级。"""
        prev_sessions: list[str] = json.loads(existing["session_ids"]) or []
        merged_sessions = list(dict.fromkeys([*prev_sessions, *incoming.session_ids]))

        prev_count = int(existing["sample_count"])
        new_weight = max(1, incoming.sample_count)
        new_count = prev_count + new_weight

        # 加权平均 success_rate
        prev_rate = float(existing["success_rate"])
        weighted = (
            (prev_rate * prev_count + incoming.success_rate * new_weight)
            / max(1, prev_count + new_weight)
        )

        # 置信度也加权
        prev_conf = float(existing["confidence"])
        new_conf = (
            (prev_conf * prev_count + incoming.confidence * new_weight)
            / max(1, prev_count + new_weight)
        )

        # 时间：last_triggered_at = max(prev, incoming, now)
        candidates = [
            t for t in (existing["last_triggered_at"], incoming.last_triggered_at)
            if t is not None
        ]
        candidates.append(_now_helper())
        last_triggered = max(candidates)

        # 稳定性评估
        target_status: str = existing["status"]
        if _is_stable(
            session_ids=merged_sessions,
            created_at=existing["created_at"],
            last_triggered_at=last_triggered,
            min_sessions=self.STABILITY_MIN_SESSIONS,
            min_span_s=self.STABILITY_MIN_SPAN_S,
            max_idle_s=self.STABILITY_MAX_IDLE_S,
        ):
            target_status = "active"

        sessions_json = json.dumps(merged_sessions, ensure_ascii=False)
        with self._lock:
            self._conn.execute(
                """
                UPDATE patterns
                SET sample_count = ?,
                    success_rate = ?,
                    confidence = ?,
                    last_triggered_at = ?,
                    session_ids = ?,
                    status = ?,
                    updated_at = ?
                WHERE id = ?
                """,
                (
                    new_count,
                    weighted,
                    new_conf,
                    last_triggered,
                    sessions_json,
                    target_status,
                    _now_helper(),
                    existing["id"],
                ),
            )
        return PatternWriteResult(
            accepted=True,
            pattern_id=existing["id"],
            reason="merged into existing pattern",
        )

    # -------- 读：Pattern candidates（Anvil 使用） --------

    async def list_pattern_candidates(
        self, min_sample_count: int = 1
    ) -> list[Pattern]:
        """列出 status='candidate' 的 patterns，供 Anvil 升级判定。"""
        return await asyncio.to_thread(self._list_candidates_sync, min_sample_count)

    def _list_candidates_sync(self, min_sample_count: int) -> list[Pattern]:
        with self._lock:
            rows = self._conn.execute(
                """
                SELECT * FROM patterns
                WHERE status = 'candidate' AND sample_count >= ?
                ORDER BY updated_at DESC
                """,
                (min_sample_count,),
            ).fetchall()
        return [self._row_to_pattern(r) for r in rows]

    async def update_pattern_status(self, pattern_id: str, new_status: str) -> bool:
        """直接更新某 pattern 的 status。Anvil 升级 / 退役 时用。"""
        return await asyncio.to_thread(self._update_pattern_status_sync, pattern_id, new_status)

    def _update_pattern_status_sync(self, pattern_id: str, new_status: str) -> bool:
        if new_status not in ("candidate", "active", "retired"):
            return False
        with self._lock:
            cur = self._conn.execute(
                "UPDATE patterns SET status = ?, updated_at = ? WHERE id = ?",
                (new_status, _now_helper(), pattern_id),
            )
            return cur.rowcount > 0

    # -------- 读 --------

    async def recent(
        self,
        n: int = 50,
        session_id: str | None = None,
    ) -> list[RawEvent]:
        """读取最近 n 条 RawEvent，按 step_number 倒序后再正序返回（时间正向）。"""
        return await asyncio.to_thread(self._recent_sync, n, session_id)

    def _recent_sync(self, n: int, session_id: str | None) -> list[RawEvent]:
        sql = "SELECT * FROM raw_events"
        params: tuple[Any, ...] = ()
        if session_id is not None:
            sql += " WHERE session_id = ?"
            params = (session_id,)
        sql += " ORDER BY step_number DESC, timestamp DESC LIMIT ?"
        params = params + (n,)
        with self._lock:
            rows = self._conn.execute(sql, params).fetchall()
        events = [self._row_to_raw(r) for r in rows]
        # 时间正向返回（旧 → 新），更符合 LLM 阅读直觉
        events.reverse()
        return events

    @staticmethod
    def _row_to_raw(row: sqlite3.Row) -> RawEvent:
        return RawEvent(
            id=row["id"],
            session_id=row["session_id"],
            task_id=row["task_id"],
            timestamp=row["timestamp"],
            step_number=row["step_number"],
            event_type=row["event_type"],
            data=json.loads(row["data"]),
        )

    async def search_facts(self, query: str, top_k: int = 10) -> list[Fact]:
        """FTS5 全文检索 facts。空查询返回最近的 top_k 条 active fact。"""
        return await asyncio.to_thread(self._search_facts_sync, query, top_k)

    def _search_facts_sync(self, query: str, top_k: int) -> list[Fact]:
        q = query.strip()
        if not q:
            with self._lock:
                rows = self._conn.execute(
                    "SELECT * FROM facts WHERE status = 'active' "
                    "ORDER BY updated_at DESC LIMIT ?",
                    (top_k,),
                ).fetchall()
            return [self._row_to_fact(r) for r in rows]

        fts_query = _sanitize_fts_query(q)
        with self._lock:
            try:
                fts_rows = self._conn.execute(
                    "SELECT rowid FROM facts_fts WHERE facts_fts MATCH ? ORDER BY rank LIMIT ?",
                    (fts_query, top_k * 4),
                ).fetchall()
            except sqlite3.OperationalError:
                fts_rows = []
            if not fts_rows:
                return []
            rowids = [r["rowid"] for r in fts_rows]
            all_active = self._conn.execute(
                "SELECT * FROM facts WHERE status = 'active'"
            ).fetchall()
        wanted = set(rowids)
        matched = [r for r in all_active if self._fts_rowid(r["id"]) in wanted]
        order = {rid: i for i, rid in enumerate(rowids)}
        matched.sort(key=lambda r: order.get(self._fts_rowid(r["id"]), 1 << 30))
        return [self._row_to_fact(r) for r in matched[:top_k]]

    @staticmethod
    def _row_to_fact(row: sqlite3.Row) -> Fact:
        return Fact(
            id=row["id"],
            subject=row["subject"],
            predicate=row["predicate"],
            object=row["object"],
            confidence=row["confidence"],
            provenance_chain=json.loads(row["provenance_chain"]),
            source_raw_event_id=row["source_raw_event_id"],
            status=row["status"],
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )

    async def patterns_for_task(
        self,
        task_description: str,
        min_confidence: float = 0.5,
    ) -> list[Pattern]:
        """按任务描述 + 置信度过滤的相关 Pattern。

        **只返回 status='active'**（candidate 不暴露给 Forge）。
        """
        return await asyncio.to_thread(
            self._patterns_for_task_sync, task_description, min_confidence
        )

    def _patterns_for_task_sync(
        self,
        task_description: str,
        min_confidence: float,
    ) -> list[Pattern]:
        q = task_description.strip()
        with self._lock:
            if not q:
                rows = self._conn.execute(
                    """
                    SELECT * FROM patterns
                    WHERE status = 'active' AND confidence >= ?
                    ORDER BY confidence DESC, sample_count DESC LIMIT 20
                    """,
                    (min_confidence,),
                ).fetchall()
                return [self._row_to_pattern(r) for r in rows]

            fts_query = _sanitize_fts_query(q)
            fts_rows = self._conn.execute(
                "SELECT rowid FROM patterns_fts WHERE patterns_fts MATCH ? ORDER BY rank LIMIT 50",
                (fts_query,),
            ).fetchall()
            if not fts_rows:
                return []
            wanted = {r["rowid"] for r in fts_rows}
            all_active = self._conn.execute(
                "SELECT * FROM patterns WHERE status = 'active' AND confidence >= ?",
                (min_confidence,),
            ).fetchall()
        matched = [r for r in all_active if self._fts_rowid(r["id"]) in wanted]
        order = {r["rowid"]: i for i, r in enumerate(fts_rows)}
        matched.sort(key=lambda r: order.get(self._fts_rowid(r["id"]), 1 << 30))
        return [self._row_to_pattern(r) for r in matched[:20]]

    @staticmethod
    def _row_to_pattern(row: sqlite3.Row) -> Pattern:
        # 注意：DB 允许 status='candidate'，但 Pattern.status Literal 只允许 'active'|'retired'。
        # 用 model_construct 绕过验证以保留 candidate 语义；调用方按需检查。
        return Pattern.model_construct(
            id=row["id"],
            pattern_type=row["pattern_type"],
            trigger_condition=row["trigger_condition"],
            action_template=json.loads(row["action_template"]),
            success_rate=row["success_rate"],
            sample_count=row["sample_count"],
            confidence=row["confidence"],
            last_triggered_at=row["last_triggered_at"],
            session_ids=json.loads(row["session_ids"]),
            status=row["status"],
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )

    # -------- 一站式上下文装配 --------

    # -------- 批量列表（用于图谱构建等） --------

    async def list_all_facts(
        self,
        status: str = "active",
        limit: int = 10000,
    ) -> list[Fact]:
        """按 status 批量取 facts（默认 active），按 updated_at 倒序，最多 limit 条。

        用于图谱构建等需要遍历全量记忆的场景。FTS5 不适合空查询；这里直接走主表。
        """
        return await asyncio.to_thread(self._list_all_facts_sync, status, limit)

    def _list_all_facts_sync(self, status: str, limit: int) -> list[Fact]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT * FROM facts WHERE status = ? "
                "ORDER BY updated_at DESC LIMIT ?",
                (status, limit),
            ).fetchall()
        return [self._row_to_fact(r) for r in rows]

    async def list_all_patterns(
        self,
        status: str = "active",
        limit: int = 10000,
    ) -> list[Pattern]:
        """按 status 批量取 patterns（默认 active），按 confidence 倒序。"""
        return await asyncio.to_thread(self._list_all_patterns_sync, status, limit)

    def _list_all_patterns_sync(self, status: str, limit: int) -> list[Pattern]:
        with self._lock:
            rows = self._conn.execute(
                "SELECT * FROM patterns WHERE status = ? "
                "ORDER BY confidence DESC, sample_count DESC LIMIT ?",
                (status, limit),
            ).fetchall()
        return [self._row_to_pattern(r) for r in rows]

    async def context_packet(
        self,
        task_description: str,
        token_budget: int = 12000,
    ) -> ContextPacket:
        """组装上下文包：patterns + facts + recent events，按 token 预算裁剪。

        策略：先收集，再按优先级（patterns → facts → recent）逐项加入直到预算耗尽。
        """
        patterns = await self.patterns_for_task(task_description)
        facts = await self.search_facts(task_description, top_k=20)
        recent = await self.recent(20)

        # 按预算分配（粗估）
        kept_patterns: list[Pattern] = []
        kept_facts: list[Fact] = []
        kept_recent: list[RawEvent] = []
        used = 0

        for p in patterns:
            cost = _estimate_tokens(
                f"{p.pattern_type}|{p.trigger_condition}|{json.dumps(p.action_template, ensure_ascii=False, default=str)}"
            )
            if used + cost > token_budget:
                break
            kept_patterns.append(p)
            used += cost

        for f in facts:
            cost = _estimate_tokens(f"{f.subject} {f.predicate} {f.object}")
            if used + cost > token_budget:
                break
            kept_facts.append(f)
            used += cost

        for r in recent:
            cost = _estimate_tokens(json.dumps(r.data, ensure_ascii=False, default=str))
            if used + cost > token_budget:
                break
            kept_recent.append(r)
            used += cost

        return ContextPacket(
            facts=kept_facts,
            patterns=kept_patterns,
            recent_events=kept_recent,
            total_tokens_estimate=used,
        )


# ============================================================
# FTS5 helpers
# ============================================================

# FTS5 保留字符；出现时需要引号包裹整个 token
_FTS_SPECIAL = set('"*():+-')


def _sanitize_fts_query(q: str) -> str:
    """把人类自然语言转换成合法的 FTS5 MATCH 查询。

    策略：
    - 拆 token（按空白）
    - 每个 token 用双引号包裹（FTS5 短语语法），消除保留字含义
    - 多 token 之间默认 AND（FTS5 默认即 AND）
    """
    tokens = [t for t in q.split() if t]
    if not tokens:
        return '""'
    quoted = []
    for t in tokens:
        # 转义内嵌引号
        safe = t.replace('"', '""')
        quoted.append(f'"{safe}"')
    return " ".join(quoted)


# ============================================================
# 稳定性检测器（L3 闸门通用规则）
# ============================================================

def _now_helper() -> float:
    """模块级时间获取，便于测试 monkeypatch。"""
    import time as _t

    return _t.time()


def _is_stable(
    *,
    session_ids: list[str],
    created_at: float | None,
    last_triggered_at: float | None,
    min_sessions: int,
    min_span_s: float,
    max_idle_s: float,
    now: float | None = None,
) -> bool:
    """稳定性判定（ARES 设计文档 5.4）。

    一个 candidate 升级为 active 当且仅当：
    1. len(session_ids) >= min_sessions（默认 >=5）
    2. last_triggered_at - created_at >= min_span_s（默认 24h）
    3. now - last_triggered_at <= max_idle_s（默认 30 天）

    没有 last_triggered_at 时直接拒绝（视为未触发）。
    """
    if last_triggered_at is None:
        return False
    if len(session_ids) < min_sessions:
        return False
    base = created_at if created_at is not None else last_triggered_at
    if last_triggered_at - base < min_span_s:
        return False
    current = now if now is not None else _now_helper()
    if current - last_triggered_at > max_idle_s:
        return False
    return True
