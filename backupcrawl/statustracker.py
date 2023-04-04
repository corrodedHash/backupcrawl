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


class Stopwatch:
    """Class to track time"""

    def __init__(self) -> None:
        self.start_time = time.time()
        self.last_lap_time = self.start_time

    @property
    def total_time(self) -> float:
        """Total time since the stopwatch started"""
        return time.time() - self.start_time

    @property
    def lap_time(self) -> float:
        """Time since the last time `lap` was called"""
        return time.time() - self.last_lap_time

    def lap(self) -> None:
        """Start new lap"""
        self.last_lap_time = time.time()


class PathTracker:
    """Keep track of information about which paths have been checked and are still being checked"""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.open_count = 0
        self.close_count = 0

        self.last_opened: str | None = None
        self.opened_first_level: list[Path] = []
        self.closed_first_level: list[Path] = []

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        self.open_count += len(paths)
        if paths:
            self.last_opened = str(paths[-1])
            if len(paths[-1].relative_to(self.root).parents) == 1:
                self.opened_first_level += paths

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.close_count += 1
        if len(path.relative_to(self.root).parents) == 1:
            self.closed_first_level.append(path)

    @property
    def open_delta(self) -> int:
        """Amount of paths that are currently being processed"""
        return self.open_count - self.close_count


class TimingStatusTracker(AbstractContextManager["TimingStatusTracker"]):
    """Trackes status of crawling"""

    def __init__(self, root: Path, console: rich.console.Console):
        self.live_display = rich.live.Live(None, console=console)
        self.live_display.start()
        self.root = root
        self.status_update_ticker = Stopwatch()

        self.path_tracker = PathTracker(root)

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
        self.path_tracker.open_paths(paths)
        self._maybe_update()

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.path_tracker.close_path(path)
        self._maybe_update()

    def _print_status(self) -> None:
        status_text = rich.text.Text()
        status_text.append(
            f"{self.status_update_ticker.total_time:>7.2f}", style="bold"
        )
        status_text.append(" ")
        status_text.append(f"{self.path_tracker.open_delta:>4}", style="#ADD8E6")
        status_text.append(" + ")
        status_text.append(f"{self.path_tracker.close_count:>6}", style="#808080")
        current_path = rich.text.Text(
            rich.markup.escape(str(self.path_tracker.last_opened))
        )

        progress_bar = rich.progress_bar.ProgressBar(
            len(self.path_tracker.opened_first_level),
            len(self.path_tracker.closed_first_level),
            width=30,
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
        if self.status_update_ticker.lap_time < 0.1:
            return
        self.status_update_ticker.lap()
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
