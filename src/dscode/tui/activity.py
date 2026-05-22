"""Activity panel widget --       MAGI         """
from __future__ import annotations

from textual.widgets import RichLog


class ActivityPanel(RichLog):
    """     / MAGI        

                 MAGI          
    """

    def __init__(self, **kwargs) -> None:
        super().__init__(highlight=True, markup=True, auto_scroll=True, **kwargs)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def add_tool_start(self, tool_name: str, args: str = "") -> None:
        """          """
        args_display = args[:80] + "..." if len(args) > 80 else args
        self.write(f"[cyan][TOOL] {tool_name}({args_display}) ...[/]")

    def add_tool_end(
        self, tool_name: str, status: str = "success", result: str = "", elapsed_ms: int = 0
    ) -> None:
        """          """
        if status == "success":
            elapsed = f" ({elapsed_ms}ms)" if elapsed_ms else ""
            self.write(f"[green][OK] {tool_name}{elapsed}[/]")
        else:
            self.write(f"[red][ERR] {tool_name}[/]")

    def add_phase_change(self, phase: str, round_num: int = 0, summary: str = "") -> None:
        """   MAGI       """
        label = f"[MAGI R{round_num}]" if round_num else ""
        text = f"{label} {phase}: {summary}" if summary else f"{label} {phase}"
        self.write(f"[magenta]{text.strip()}[/]")
