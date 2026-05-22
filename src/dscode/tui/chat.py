"""Chat panel widget --         

   RichLog        LLM           
"""
from __future__ import annotations

from textual.widgets import RichLog


class ChatPanel(RichLog):
    """       

       write()          (RichLog       
           chunk                
    """

    def __init__(self, **kwargs) -> None:
        super().__init__(highlight=True, markup=True, auto_scroll=True, **kwargs)
        self._streaming: bool = False

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def add_user_message(self, text: str) -> None:
        """       """
        self.write(f"[bold cyan]>[/] {text}")

    def add_assistant_stream(self, text: str) -> None:
        """   LLM      chunk 

           chunk        chunk      
        """
        if not self._streaming:
            self.write(f"[bold green]<[/] {text}")
            self._streaming = True
        else:
            self.write(text)

    def add_assistant_end(self) -> None:
        """     LLM         stream        """
        self._streaming = False

    def add_system(self, text: str) -> None:
        """         """
        self.write(f"[dim]System: {text}[/]")
