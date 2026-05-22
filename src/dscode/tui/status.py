"""Status bar widget --       

                  MAGI   /   
      .dscode/telemetry.json          
"""
from __future__ import annotations

import json
from pathlib import Path

from textual.widgets import Static


class StatusBar(Static):
    """      

          :
    1. render_status(...) --   SessionEvent   
    2. refresh_from_file(path) --     telemetry.json   
    """

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def render_status(
        self,
        model: str = "",
        cache_hit_rate: float = 0.0,
        cost_saved: float = 0.0,
        round_num: int = 0,
        phase: str = "idle",
        call_count: int = 0,
    ) -> None:
        """  SessionEvent(STATUS_UPDATE)        """
        #           "  "
        if (
            not model
            and cache_hit_rate == 0.0
            and cost_saved == 0.0
            and round_num == 0
            and phase == "idle"
            and call_count == 0
        ):
            self.update("[reverse] DS Code | ready [/]")
            return

        model_display = model or "-"
        cache_pct = f"{cache_hit_rate * 100:.0f}" if cache_hit_rate else "0"
        saved = f"CNY{cost_saved:.2f}" if cost_saved else "CNY0.00"
        round_label = f"R{round_num}" if round_num else "-"

        line = (
            f"[reverse] DS Code | {model_display} | "
            f"Cache: {cache_pct}% | Saved: {saved} | "
            f"{round_label} | {phase} [/]"
        )
        self.update(line)

    def refresh_from_file(self, telemetry_path: Path) -> None:
        """  .dscode/telemetry.json            """
        tp = Path(telemetry_path)
        try:
            data = json.loads(tp.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError, ValueError):
            return

        try:
            model = str(data.get("model") or "")
            cache_hit_rate = float(data.get("hit_rate") or 0.0)
            cost_saved = float(data.get("total_saved_cny") or 0.0)
            round_num = int(data.get("current_round") or 0)
            phase = str(data.get("current_phase") or "idle")
            call_count = int(data.get("call_count") or 0)
        except (TypeError, ValueError):
            return

        self.render_status(
            model=model,
            cache_hit_rate=cache_hit_rate,
            cost_saved=cost_saved,
            round_num=round_num,
            phase=phase,
            call_count=call_count,
        )
