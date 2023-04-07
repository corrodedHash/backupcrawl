"""Contains ResultPrinter"""
from typing import Any
import rich
import rich.box
import rich.console
import rich.panel
import rich.style
import rich.columns

from backupcrawl.crawlresult import CrawlResult
from backupcrawl.sync_status import BackupEntry, SyncStatus


class SyncPanel(rich.panel.Panel):
    """Panel summarizing a subset of backup entries"""

    def __init__(self, name: str, entries: list[BackupEntry], show_clean: bool) -> None:
        self._status_string_map = {
            SyncStatus.DIRTY: "Dirty",
            SyncStatus.AHEAD: "Unsynced",
            SyncStatus.CLEAN: "Clean",
        }
        self._border_style_map = {
            SyncStatus.DIRTY: rich.style.Style(color="dark_red"),
            SyncStatus.AHEAD: rich.style.Style(color="pale_violet_red1"),
            SyncStatus.CLEAN: rich.style.Style(color="green"),
        }
        desired_sync_states = [SyncStatus.DIRTY, SyncStatus.AHEAD]

        if show_clean:
            desired_sync_states.append(SyncStatus.CLEAN)

        filtered_entries = {
            enum_state: [t for t in entries if t.status == enum_state]
            for enum_state in desired_sync_states
        }

        filtered_states = [x for x in desired_sync_states if filtered_entries[x]]
        self.display_count = sum(
            len(x)
            for x in [filtered_entries[enum_state] for enum_state in filtered_states]
        )
        result_panels = [
            rich.panel.Panel(
                "\n".join([str(x.path) for x in filtered_entries[enum_state]]),
                border_style=self._border_style_map[enum_state],
                title=self._status_string_map[enum_state],
                title_align="left",
            )
            for enum_state in filtered_states
        ]
        super().__init__(
            rich.console.Group(*result_panels),
            title=name,
            title_align="left",
        )


class ResultPrinter:
    """Prints crawl results"""

    def __init__(self, console: rich.console.Console):
        self.console = console

    def print(self, crawl_result: CrawlResult, show_clean: bool = False) -> None:
        """Prints result of crawl"""

        loose_paths = rich.panel.Panel(
            "\n".join([str(x) for x in crawl_result.loose_paths]),
            title="Not backed up",
            title_align="left",
        )

        denied_paths = rich.panel.Panel(
            "\n".join([str(x) for x in crawl_result.denied_paths]),
            title="Permission denied",
            title_align="left",
        )

        output: list[Any] = []
        if crawl_result.loose_paths:
            output.append(loose_paths)
        if crawl_result.denied_paths:
            output.append(denied_paths)
        sync_panels = [
            SyncPanel(backup_type.name(), crawl_result.backups[backup_type], show_clean)
            for backup_type in crawl_result.backups
        ]
        output.append(
            rich.console.Group(*[x for x in sync_panels if x.display_count > 0])
        )
        self.console.print(
            rich.panel.Panel.fit(
                rich.console.Group(*output), box=rich.box.SIMPLE_HEAD, padding=(0, 0)
            )
        )
