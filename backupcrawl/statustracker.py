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


class PathTrackerDisplay(rich.live.Live):
    """Progress display for crawling in directory using `PathTracker`"""
    def __init__(self, path_tracker: PathTracker, console: rich.console.Console):
        self.progress_bar = rich.progress.Progress(
            rich.progress.BarColumn(),
            rich.progress.TimeElapsedColumn(),
            rich.progress.TextColumn("{task.fields[open_delta]:>4}", style="#ADD8E6"),
            rich.progress.TextColumn(" + "),
            rich.progress.TextColumn("{task.fields[close_count]}", style="#808080"),
        )
        self.progress_bar_task = self.progress_bar.add_task("Crawling", start=True)
        self.progress_path = rich.text.Text()
        self.path_tracker = path_tracker
        super().__init__(
            rich.console.Group(self.progress_bar, self.progress_path),
            console=console,
            refresh_per_second=8
        )

    def refresh(self) -> None:
        self.progress_bar.update(
            self.progress_bar_task,
            total=len(self.path_tracker.opened_first_level),
            completed=len(self.path_tracker.closed_first_level),
            open_delta=self.path_tracker.open_delta,
            close_count=self.path_tracker.close_count,
        )

        self.progress_path.truncate(0)
        if self.path_tracker.last_opened is not None:
            self.progress_path.append(
                rich.markup.escape(str(self.path_tracker.last_opened))
            )
        return super().refresh()


class TimingStatusTracker(AbstractContextManager["TimingStatusTracker"]):
    """Trackes status of crawling"""

    def __init__(self, root: Path, console: rich.console.Console):
        self.root = root
        self.status_update_ticker = Stopwatch()

        self.path_tracker = PathTracker(root)
        self.progress = PathTrackerDisplay(self.path_tracker, console)

    def __enter__(self) -> Self:
        self.progress.__enter__()
        return self

    def __exit__(
        self,
        exc_type: None | type[BaseException],
        exc_val: None | BaseException,
        exc_tb: None | TracebackType,
    ) -> None:
        """Notify the status tracker, that crawling has stopped"""
        self.path_tracker.last_opened = None
        self.progress.__exit__(exc_type, exc_val, exc_tb)

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        self.path_tracker.open_paths(paths)
        self._maybe_update()

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.path_tracker.close_path(path)
        self._maybe_update()

    def _maybe_update(self) -> None:
        """Prints current status"""
        if self.status_update_ticker.lap_time < 0.1:
            return
        self.status_update_ticker.lap()


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
