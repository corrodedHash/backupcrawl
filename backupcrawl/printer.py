"""Contains ResultPrinter"""
from typing import Any
import rich
import rich.box
import rich.console
import rich.panel
import rich.style

from backupcrawl.crawlresult import CrawlResult
from backupcrawl.sync_status import SyncStatus


class ResultPrinter:
    """Prints crawl results"""

    def __init__(self, console: rich.console.Console):
        self.console = console

    def print(self, crawl_result: CrawlResult, show_clean: bool = False) -> None:
        """Prints result of crawl"""

        loose_paths = rich.panel.Panel.fit(
            "\n".join([str(x) for x in crawl_result.loose_paths])
        )
        loose_paths.title = "Not backed up"
        loose_paths.title_align = "left"

        denied_paths = rich.panel.Panel.fit(
            "\n".join([str(x) for x in crawl_result.denied_paths])
        )
        denied_paths.title = "Permission denied"
        denied_paths.title_align = "left"

        output: list[Any] = [loose_paths]
        if crawl_result.denied_paths:
            output.append(denied_paths)
        output.append(self._get_sync_panels(crawl_result, show_clean))
        self.console.print(*output)

    def _get_sync_panels(
        self, crawl_result: CrawlResult, show_clean: bool
    ) -> rich.console.Group:
        desired_sync_states: list[tuple[SyncStatus, str, rich.style.Style]] = [
            (SyncStatus.DIRTY, "Dirty", rich.style.Style(color="dark_red")),
            (SyncStatus.AHEAD, "Unsynced", rich.style.Style(color="pale_violet_red1")),
        ]
        if show_clean:
            desired_sync_states.append(
                (SyncStatus.CLEAN, "Clean", rich.style.Style(color="green"))
            )

        sync_panels: list[rich.panel.Panel] = []
        for backup_type in crawl_result.backups:
            result_panels: list[rich.panel.Panel] = []
            for enum_state, status_string, border_style in desired_sync_states:
                filtered_paths = [
                    t.path
                    for t in crawl_result.backups[backup_type]
                    if t.status == enum_state
                ]
                new_panel = rich.panel.Panel(
                    "\n".join([str(x) for x in filtered_paths]),
                    border_style=border_style,
                )
                new_panel.title = status_string
                new_panel.title_align = "left"

                result_panels.append(new_panel)
            new_sync_panel = rich.panel.Panel.fit(rich.console.Group(*result_panels))
            new_sync_panel.title = backup_type.name()
            new_sync_panel.title_align = "left"

            sync_panels.append(new_sync_panel)
        all_sync_panels = rich.console.Group(*sync_panels)
        return all_sync_panels
