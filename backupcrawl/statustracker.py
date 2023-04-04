"""Contains StatusTracker class"""
import time
from contextlib import AbstractContextManager
from pathlib import Path
from types import TracebackType

import rich.console
import rich.live
import rich.markup
import rich.text
import rich.progress
import rich.progress_bar
from typing_extensions import Self


class TimingStatusTracker(AbstractContextManager["TimingStatusTracker"]):
    """Trackes status of crawli ng"""

    def __init__(self, root: Path, console: rich.console.Console):
        self.live_display = rich.live.Live(None, console=console)
        self.live_display.start()
        self.root = root
        self.open_count = 0
        self.close_count = 0
        self.start_time = time.time()
        self.last_status_time = self.start_time
        self.last_opened: str | None = None

        self.opened_first_level: list[Path] = []
        self.closed_first_level: list[Path] = []

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: None | type[BaseException],
        exc_val: None | BaseException,
        exc_tb: None | TracebackType,
    ) -> None:
        """Notify the status tracker, that crawling has stopped"""
        self._print_status()
        self.live_display.stop()

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        self.open_count += len(paths)
        if paths:
            self.last_opened = str(paths[-1])
            if len(paths[-1].relative_to(self.root).parents) == 1:
                self.opened_first_level += paths
        self._maybe_update()

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.close_count += 1
        if len(path.relative_to(self.root).parents) == 1:
            self.closed_first_level.append(path)
        self._maybe_update()

    @property
    def runtime(self) -> float:
        """Returns the time the tracker has been running"""
        return self.last_status_time - self.start_time

    def _print_status(self) -> None:
        status_text = rich.text.Text()
        status_text.append(f"{self.runtime:>7.2f}", style="bold")
        status_text.append(" ")
        status_text.append(f"{self.open_count - self.close_count:>4}", style="#ADD8E6")
        status_text.append(" + ")
        status_text.append(f"{self.close_count:>6}", style="#808080")
        current_path = rich.text.Text(rich.markup.escape(str(self.last_opened)))

        progress_bar = rich.progress_bar.ProgressBar(
            len(self.opened_first_level), len(self.closed_first_level), width=30
        )

        self.live_display.update(
            rich.console.Group(
                progress_bar,
                status_text,
                current_path,
            ),
            refresh=True,
        )

    def _maybe_update(self) -> None:
        """Prints current status"""
        if time.time() - self.last_status_time < 0.1:
            return
        self.last_status_time = time.time()
        self._print_status()


class VoidStatusTracker:
    """Provides tracker interface, outputs nothing"""

    def __init__(self, path: Path):
        pass

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""

    def close_path(self, path: Path) -> None:
        """Prints current status"""

    def __enter__(self) -> Self:
        return self

    def __exit__(self, exc_type: None, exc_val: None, exc_tb: None) -> None:
        """Notify the status tracker, that crawling has stopped"""


StatusTracker = TimingStatusTracker | VoidStatusTracker
