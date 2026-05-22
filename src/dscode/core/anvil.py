"""Anvil —— 异步反思引擎（v2 实现）。

参见 `ARES 新范式设计.md` 第五章。

设计原则：
1. **完全异步、非热路径**——Anvil 跑慢、Anvil 崩了都不影响 Forge。
2. **多阶段工程化管线**，不是一次 LLM 调用：
   关键帧检测 → 间歇摘要 → 事实提取 → 模式归纳 → 矛盾检测 → 升级决策
3. **可独立审查的输出**——CompressionReport / ReflectionReport / Contradiction 都是 dataclass。
4. **LLM 失败容忍**——任何 LLM 调用失败都返回保底输出，不抛异常。

四个核心方法 + 一站式入口：
- compress_session:    L1 raw → L2 facts（关键帧 + 摘要 + 三元组）
- extract_patterns:    L2 facts → L3 pattern candidates（频次统计 + LLM 归纳）
- detect_contradictions: 同 (subject, predicate) 不同 object 的 fact 对
- promote_candidates:  candidate → active（稳定性检测器）
- run_full_reflection: 串联以上四步，返回 ReflectionReport
"""
from __future__ import annotations

import json
import time
from collections import defaultdict
from typing import Any

from pydantic import BaseModel, Field

from dscode.core.types import (
    Fact,
    LLMProviderProtocol,
    Message,
    Pattern,
    RawEvent,
    ScribeProtocol,
)

# ============================================================
# Anvil 专属数据类（按指令不写到 types.py）
# ============================================================


class CompressionReport(BaseModel):
    """单 session 压缩管线的结果汇报。"""

    session_id: str
    raw_events_processed: int = 0
    facts_extracted: int = 0
    facts_accepted: int = 0  # 通过 L2 闸门的
    elapsed_ms: int = 0
    notes: list[str] = Field(default_factory=list)


class Contradiction(BaseModel):
    """两条事实在 (subject, predicate) 上一致但 object 矛盾。"""

    fact_a_id: str
    fact_b_id: str
    reason: str


class ReflectionReport(BaseModel):
    """一次完整 reflect 的输出，可以序列化、落盘、审计。"""

    session_id: str | None = None
    compression: CompressionReport | None = None
    patterns_extracted: int = 0
    patterns_promoted: int = 0
    contradictions_found: int = 0
    elapsed_ms: int = 0
    notes: list[str] = Field(default_factory=list)


# ============================================================
# 内部辅助 - LLM 调用兜底
# ============================================================


async def _safe_force_json(
    llm: LLMProviderProtocol,
    *,
    system_prompt: str,
    user_prompt: str,
    schema_hint: str,
    model: str,
    fallback: dict[str, Any],
) -> dict[str, Any]:
    """对 LLM 调用做异常隔离，并优先使用 DeepSeekClient 的 force_json。"""
    # 延迟 import，避免必须依赖 deepseek
    try:
        from dscode.deepseek.client import DeepSeekClient  # type: ignore
        from dscode.deepseek.prefix_completion import force_json  # type: ignore

        if isinstance(llm, DeepSeekClient):
            try:
                return await force_json(
                    client=llm,
                    schema_hint=schema_hint,
                    model=model,
                    user_prompt=user_prompt,
                    system_prompt=system_prompt,
                    max_tokens=2048,
                )
            except Exception:
                return fallback
    except Exception:
        pass

    # 通用回退：普通 chat() + 宽松解析
    try:
        resp = await llm.chat(
            messages=[
                Message(role="system", content=system_prompt),
                Message(role="user", content=user_prompt),
            ],
            model=model,
        )
        return _loose_parse_json(resp.content or "") or fallback
    except Exception:
        return fallback


def _loose_parse_json(text: str) -> dict[str, Any] | None:
    """宽松解析：先尝试整段，再抽取首个 {...} 块。"""
    s = text.strip()
    if not s:
        return None
    # 去除 markdown 围栏
    if s.startswith("```"):
        s = s.strip("`")
        if s.startswith("json"):
            s = s[4:]
    try:
        obj = json.loads(s)
        return obj if isinstance(obj, dict) else None
    except Exception:
        pass
    # 抽取最外层 {...}
    start = s.find("{")
    end = s.rfind("}")
    if start >= 0 and end > start:
        try:
            obj = json.loads(s[start : end + 1])
            return obj if isinstance(obj, dict) else None
        except Exception:
            return None
    return None


# ============================================================
# Anvil 类
# ============================================================


_SYSTEM_COMPRESS = """\
你是 Anvil 的压缩助手。你的任务是把一段连续成功的工具调用序列总结为一两句话。
要求：
- 不超过 80 个汉字 / 100 个 ASCII。
- 突出"读了什么文件 / 跑了什么命令 / 得到了什么结论"。
- 不要复述事件。
严格输出 JSON: {"summary": "..."}
"""

_SYSTEM_EXTRACT_FACT = """\
你是 Anvil 的事实抽取助手。从给定事件中提取 0-3 条 (subject, predicate, object) 三元组。
要求：
- subject / predicate / object 都是简短的字符串
- 不要泛泛而谈，事实要可被定位到具体文件、命令或结果
- 没有可抽取事实时返回空列表
严格输出 JSON: {"facts": [{"subject": "...", "predicate": "...", "object": "..."}, ...]}
"""

_SYSTEM_PATTERN = """\
你是 Anvil 的模式归纳助手。给定一组在多个 session 中重复出现的事实，归纳为 1 个 Pattern。
要求：
- pattern_type 从 ['sop','rule','preference','heuristic','skill','concept'] 中选一个
- trigger_condition 是"在什么情况下触发"，一句话
- action_template 是 dict，描述对应的标准动作或观察
严格输出 JSON: {"pattern_type": "...", "trigger_condition": "...", "action_template": {...}, "confidence": 0.0-1.0}
"""


class Anvil:
    """异步反思引擎。

    Args:
        scribe: Scribe 实例（或实现 ScribeProtocol 的 mock）。
        llm: LLMProviderProtocol（用于压缩、事实抽取、模式归纳）。
        compress_model: 用于压缩 / 抽取的模型名（默认 deepseek-v4-flash，便宜）。
    """

    # 触发关键帧的事件类型
    _KEYFRAME_EVENT_TYPES = {"error", "safety_block"}
    # 工具结果带 status=error 也是关键帧（在 _is_keyframe 中处理）

    def __init__(
        self,
        scribe: ScribeProtocol,
        llm: LLMProviderProtocol,
        compress_model: str = "deepseek-v4-flash",
    ) -> None:
        self.scribe = scribe
        self.llm = llm
        self.compress_model = compress_model

    # ------------------------------------------------------------
    # 1) 压缩管道：L1 → L2
    # ------------------------------------------------------------

    async def compress_session(self, session_id: str) -> CompressionReport:
        """把一个 session 的 raw_events 压缩成 facts 并写入 Scribe。

        步骤：
        1. recent(n=200, session_id) 取事件
        2. 关键帧检测（无 LLM）
        3. 间歇摘要（关键帧之间的连续成功段调 LLM 浓缩）
        4. 事实提取（LLM 三元组）
        5. write_fact 落库
        """
        t0 = time.time()
        events = await self.scribe.recent(n=200, session_id=session_id)
        events = sorted(events, key=lambda e: e.step_number)

        report = CompressionReport(
            session_id=session_id,
            raw_events_processed=len(events),
        )

        if not events:
            report.elapsed_ms = int((time.time() - t0) * 1000)
            report.notes.append("session 无事件")
            return report

        # 阶段 1：关键帧
        key_indices = [i for i, e in enumerate(events) if _is_keyframe(e)]

        # 阶段 2：间歇摘要——成功的 tool_call/tool_result 段
        summaries: list[tuple[list[RawEvent], str]] = []
        if not key_indices:
            # 没有关键帧，整段当成一个摘要候选（但只对足够长的段做）
            if len(events) >= 3:
                summary = await self._summarize_segment(events)
                if summary:
                    summaries.append((events, summary))
        else:
            prev = 0
            for kf in key_indices:
                segment = events[prev:kf]
                if len(segment) >= 2:
                    summary = await self._summarize_segment(segment)
                    if summary:
                        summaries.append((segment, summary))
                prev = kf + 1
            tail = events[prev:]
            if len(tail) >= 2:
                summary = await self._summarize_segment(tail)
                if summary:
                    summaries.append((tail, summary))

        # 阶段 3 & 4：提取事实并写入
        facts_to_write: list[Fact] = []

        # 3a. 关键帧 → 事实
        for idx in key_indices:
            extracted = await self._extract_facts_from_event(events[idx])
            facts_to_write.extend(extracted)

        # 3b. 摘要 → 事实
        for segment, summary in summaries:
            extracted = await self._extract_facts_from_summary(
                summary=summary,
                provenance=[e.id for e in segment[:5]],  # 取前 5 个 raw id 当 provenance
            )
            facts_to_write.extend(extracted)

        report.facts_extracted = len(facts_to_write)

        # 写库
        for f in facts_to_write:
            try:
                result = await self.scribe.write_fact(f)
                if result.accepted:
                    report.facts_accepted += 1
            except Exception as exc:
                report.notes.append(f"write_fact error: {type(exc).__name__}: {exc}")

        report.elapsed_ms = int((time.time() - t0) * 1000)
        if not summaries:
            report.notes.append("no_summaries_generated")
        return report

    async def _summarize_segment(self, segment: list[RawEvent]) -> str:
        """对一段连续成功事件用 LLM 压缩成一句话。"""
        if not segment:
            return ""
        # 构造 LLM 输入
        head = "\n".join(
            f"[step {e.step_number}] [{e.event_type}] {json.dumps(e.data, ensure_ascii=False, default=str)[:160]}"
            for e in segment[:20]
        )
        payload = await _safe_force_json(
            self.llm,
            system_prompt=_SYSTEM_COMPRESS,
            user_prompt=f"事件序列：\n{head}\n\n请总结。",
            schema_hint='{"summary": "..."}',
            model=self.compress_model,
            fallback={"summary": ""},
        )
        return str(payload.get("summary") or "").strip()

    async def _extract_facts_from_event(self, event: RawEvent) -> list[Fact]:
        """从关键帧事件中提取事实（带 provenance）。"""
        user = (
            f"事件类型: {event.event_type}\n"
            f"事件 step: {event.step_number}\n"
            f"事件内容: {json.dumps(event.data, ensure_ascii=False, default=str)[:400]}\n"
        )
        payload = await _safe_force_json(
            self.llm,
            system_prompt=_SYSTEM_EXTRACT_FACT,
            user_prompt=user,
            schema_hint='{"facts": [{"subject": "...", "predicate": "...", "object": "..."}, ...]}',
            model=self.compress_model,
            fallback={"facts": []},
        )
        raw_facts = payload.get("facts") or []
        out: list[Fact] = []
        for rf in raw_facts:
            if not isinstance(rf, dict):
                continue
            subj = str(rf.get("subject") or "").strip()
            pred = str(rf.get("predicate") or "").strip()
            obj = str(rf.get("object") or "").strip()
            if not (subj and pred and obj):
                continue
            out.append(
                Fact(
                    subject=subj,
                    predicate=pred,
                    object=obj,
                    confidence=0.7,
                    provenance_chain=[event.id],
                    source_raw_event_id=event.id,
                )
            )
        return out

    async def _extract_facts_from_summary(
        self, *, summary: str, provenance: list[str]
    ) -> list[Fact]:
        """从摘要中抽取事实。provenance 指向被压缩的 raw_event_id 列表。"""
        if not summary:
            return []
        user = f"摘要内容: {summary}\n请提取 0-3 条三元组事实。"
        payload = await _safe_force_json(
            self.llm,
            system_prompt=_SYSTEM_EXTRACT_FACT,
            user_prompt=user,
            schema_hint='{"facts": [{"subject": "...", "predicate": "...", "object": "..."}, ...]}',
            model=self.compress_model,
            fallback={"facts": []},
        )
        raw_facts = payload.get("facts") or []
        out: list[Fact] = []
        for rf in raw_facts:
            if not isinstance(rf, dict):
                continue
            subj = str(rf.get("subject") or "").strip()
            pred = str(rf.get("predicate") or "").strip()
            obj = str(rf.get("object") or "").strip()
            if not (subj and pred and obj):
                continue
            out.append(
                Fact(
                    subject=subj,
                    predicate=pred,
                    object=obj,
                    confidence=0.6,  # 摘要派生的置信度略低
                    provenance_chain=list(provenance) or ["anvil-summary"],
                    source_raw_event_id=provenance[0] if provenance else None,
                )
            )
        return out

    # ------------------------------------------------------------
    # 2) 模式提取：L2 → L3 candidate
    # ------------------------------------------------------------

    async def extract_patterns(self, since: float | None = None) -> list[Pattern]:
        """从 L2 facts 中归纳重复模式，写入为 candidate。

        策略：
        - 取所有 active fact
        - 按 (predicate, object) 分桶
        - 桶大小 >= 3 → 当成候选模式
        - 用 LLM 把"在 N 个 fact 中观察到 X"归纳为 Pattern 对象
        """
        # 读所有 active facts
        facts = await self._collect_active_facts()
        if since is not None:
            facts = [f for f in facts if f.created_at >= since]

        if not facts:
            return []

        # 按 (predicate, object) 分桶；同时收集 sessions（从 provenance 反查）
        buckets: dict[tuple[str, str], list[Fact]] = defaultdict(list)
        for f in facts:
            buckets[(f.predicate, f.object)].append(f)

        promoted: list[Pattern] = []
        for (pred, obj), group in buckets.items():
            if len(group) < 3:
                continue
            # 调 LLM 归纳成 Pattern
            sample_subjects = sorted({g.subject for g in group})[:8]
            user = (
                f"以下事实在 {len(group)} 条记录中重复出现：\n"
                f"predicate={pred}\nobject={obj}\nsubjects={sample_subjects}\n\n"
                "请归纳成一个 Pattern。"
            )
            payload = await _safe_force_json(
                self.llm,
                system_prompt=_SYSTEM_PATTERN,
                user_prompt=user,
                schema_hint=(
                    '{"pattern_type": "...", "trigger_condition": "...", '
                    '"action_template": {...}, "confidence": 0.0-1.0}'
                ),
                model=self.compress_model,
                fallback={
                    "pattern_type": "rule",
                    "trigger_condition": f"{pred} -> {obj}",
                    "action_template": {"observation": f"{pred} {obj}"},
                    "confidence": 0.5,
                },
            )

            ptype = str(payload.get("pattern_type") or "rule")
            if ptype not in {"sop", "rule", "preference", "heuristic", "skill", "concept"}:
                ptype = "rule"
            trigger = str(payload.get("trigger_condition") or f"{pred} -> {obj}")[:300]
            action = payload.get("action_template")
            if not isinstance(action, dict):
                action = {"observation": f"{pred} {obj}"}
            try:
                conf = float(payload.get("confidence", 0.5))
            except (TypeError, ValueError):
                conf = 0.5

            pattern = Pattern.model_construct(
                pattern_type=ptype,  # type: ignore[arg-type]
                trigger_condition=trigger,
                action_template=action,
                success_rate=0.0,
                sample_count=len(group),
                confidence=conf,
                session_ids=[],  # 由 promote 阶段或后续 reinforce 填充
                status="candidate",
                last_triggered_at=time.time(),
            )
            # 兜底为合法 Pattern；用 model_construct 容忍 status='candidate'
            result = await self.scribe.write_pattern(pattern)
            if result.accepted:
                promoted.append(pattern)

        return promoted

    async def _collect_active_facts(self) -> list[Fact]:
        """收集所有 active fact。优先用 list_all_facts；不存在则降级用 search_facts。"""
        list_all = getattr(self.scribe, "list_all_facts", None)
        if callable(list_all):
            try:
                return await list_all(status="active", limit=10000)
            except Exception:
                pass
        # 兼容兜底
        return await self.scribe.search_facts("", top_k=10000)

    # ------------------------------------------------------------
    # 3) 矛盾检测
    # ------------------------------------------------------------

    async def detect_contradictions(self) -> list[Contradiction]:
        """扫描 L2 facts，找出 (subject, predicate) 一致但 object 不同的对。

        v1 用规则；不调 LLM 做语义判断。
        """
        facts = await self._collect_active_facts()
        if not facts:
            return []

        # 按 (subject, predicate) 分桶
        buckets: dict[tuple[str, str], list[Fact]] = defaultdict(list)
        for f in facts:
            buckets[(f.subject, f.predicate)].append(f)

        contradictions: list[Contradiction] = []
        for (subj, pred), group in buckets.items():
            if len(group) < 2:
                continue
            # 找出 object 不同的 pair（去重）
            seen_pairs: set[tuple[str, str]] = set()
            for i in range(len(group)):
                for j in range(i + 1, len(group)):
                    a, b = group[i], group[j]
                    if a.object == b.object:
                        continue
                    pair_key = tuple(sorted([a.id, b.id]))
                    if pair_key in seen_pairs:
                        continue
                    seen_pairs.add(pair_key)
                    contradictions.append(
                        Contradiction(
                            fact_a_id=a.id,
                            fact_b_id=b.id,
                            reason=(
                                f"同 ({subj}, {pred}) 但 object 不同: "
                                f"'{a.object[:60]}' vs '{b.object[:60]}'"
                            ),
                        )
                    )
        return contradictions

    # ------------------------------------------------------------
    # 4) 升级 candidate → active
    # ------------------------------------------------------------

    async def promote_candidates(self) -> list[Pattern]:
        """对所有 candidate 应用稳定性规则。

        升级条件（与 Scribe._is_stable 一致）：
        - len(session_ids) >= 5
        - max(triggered) - created_at >= 24h
        - now - last_triggered_at <= 30 天

        不满足且 last_triggered_at 太老 → 退役 (status='retired')。
        """
        candidates = await self.scribe.list_pattern_candidates()
        promoted: list[Pattern] = []
        now = time.time()
        max_idle_s = 30 * 24 * 3600
        min_sessions = 5
        min_span_s = 24 * 3600

        update_status = getattr(self.scribe, "update_pattern_status", None)

        for c in candidates:
            n_sessions = len(c.session_ids)
            last = c.last_triggered_at or 0
            span = last - (c.created_at or last)
            idle = now - last if last else 1e18

            if n_sessions >= min_sessions and span >= min_span_s and idle <= max_idle_s:
                # 升级
                ok = False
                if callable(update_status):
                    try:
                        ok = await update_status(c.id, "active")
                    except Exception:
                        ok = False
                if ok:
                    promoted_pattern = c.model_copy(update={"status": "active"})
                    promoted.append(promoted_pattern)
            elif idle > max_idle_s and last > 0:
                # 退役
                if callable(update_status):
                    try:
                        await update_status(c.id, "retired")
                    except Exception:
                        pass

        return promoted

    # ------------------------------------------------------------
    # 5) 一站式
    # ------------------------------------------------------------

    async def run_full_reflection(
        self, session_id: str | None = None
    ) -> ReflectionReport:
        """跑完整反思管线，返回 ReflectionReport。"""
        t0 = time.time()
        report = ReflectionReport(session_id=session_id)

        if session_id is not None:
            try:
                report.compression = await self.compress_session(session_id)
            except Exception as exc:
                report.notes.append(f"compress error: {type(exc).__name__}: {exc}")

        try:
            extracted = await self.extract_patterns()
            report.patterns_extracted = len(extracted)
        except Exception as exc:
            report.notes.append(f"extract error: {type(exc).__name__}: {exc}")

        try:
            contradictions = await self.detect_contradictions()
            report.contradictions_found = len(contradictions)
        except Exception as exc:
            report.notes.append(f"contradiction error: {type(exc).__name__}: {exc}")

        try:
            promoted = await self.promote_candidates()
            report.patterns_promoted = len(promoted)
        except Exception as exc:
            report.notes.append(f"promote error: {type(exc).__name__}: {exc}")

        report.elapsed_ms = int((time.time() - t0) * 1000)
        return report

    # ------------------------------------------------------------
    # 兼容：旧 API（保留以避免破坏其他模块潜在引用）
    # ------------------------------------------------------------

    async def reflect(self, session_id: str | None = None) -> list[Pattern]:
        """旧入口；调度到 run_full_reflection + extract_patterns。"""
        await self.run_full_reflection(session_id=session_id)
        # 返回新归纳的 candidate（兼容旧测试期望）
        return await self.extract_patterns()

    async def reinforce(self, pattern_id: str, success: bool) -> None:
        """根据执行结果反馈强化或衰减 Pattern。v1 仅记录到日志。"""
        # 简单实现：通过 list_pattern_candidates / 直接 SQL 调整 success_rate
        # 留 TODO，因为 ScribeProtocol 没暴露通用 update 接口
        # v2 实现见 ARES 5.6。
        return None

    async def deprecate_contradicted(self) -> int:
        """扫描矛盾事实并标记被推翻的旧事实为 deprecated。

        v1 简化：仅返回找到的矛盾对数量；标记动作留 v2.5（需 Scribe 暴露 update_fact_status）。
        """
        return len(await self.detect_contradictions())


# ============================================================
# 模块级辅助
# ============================================================


def _is_keyframe(event: RawEvent) -> bool:
    """关键帧判定：
    - 显式 error / safety_block 事件
    - tool_result 且 data.status == 'error'
    """
    if event.event_type in {"error", "safety_block"}:
        return True
    if event.event_type == "tool_result":
        status = str(event.data.get("status") or "").lower()
        if status == "error":
            return True
    return False


__all__ = [
    "Anvil",
    "CompressionReport",
    "Contradiction",
    "ReflectionReport",
]
