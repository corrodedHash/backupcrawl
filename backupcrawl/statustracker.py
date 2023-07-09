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
import rich.table
from typing_extensions import Self


class PathTracker:
    """Keep track of information about which paths have been checked and are still being checked"""

    def __init__(self, root: Path, straggler_time_ms: int = 1_000) -> None:
        self.root = root
        self.open_count = 0
        self.close_count = 0

        self.last_opened: str | None = None
        self.opened_first_level: list[Path] = []
        self.closed_first_level: list[Path] = []

        self.current_tree: list[tuple[Path, int]] = []
        self.stragglers: list[tuple[Path, int]] = []
        self.straggler_time = straggler_time_ms

    def current_path(self, path: Path) -> None:
        """Event to current path"""
        relative_path = path.relative_to(self.root)
        if not self.current_tree or self.current_tree[-1][0] != relative_path:
            assert (
                not self.current_tree
                or relative_path.parent == self.current_tree[-1][0]
            )
            self.current_tree.append((relative_path, time.time_ns() // (10**6)))

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        if not paths:
            return
        self.open_count += len(paths)
        self.last_opened = str(paths[-1])
        relative_path = paths[-1].relative_to(self.root)

        if len(relative_path.parents) == 1:
            self.opened_first_level += paths

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.close_count += 1
        relative_path = path.relative_to(self.root)
        if self.current_tree[-1][0] == relative_path:
            time_delta = time.time_ns() // (10**6) - self.current_tree[-1][1]
            for straggler in self.stragglers[::-1]:
                if not straggler[0].is_relative_to(relative_path):
                    break
                time_delta -= straggler[1]
            if time_delta > self.straggler_time:
                self.stragglers.append((relative_path, time_delta))
            self.current_tree.pop()
        if len(relative_path.parents) == 1:
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
        self.current_path = rich.text.Text()
        self.stragglers = rich.table.Table("Path", "Duration", title="Stragglers")
        self.progress_path = rich.text.Text()
        self.path_tracker = path_tracker
        super().__init__(
            rich.console.Group(
                self.progress_bar,
                # self.current_path,
                self.stragglers,
                self.progress_path,
            ),
            console=console,
            refresh_per_second=8,
        )

    def refresh(self) -> None:
        self.progress_bar.update(
            self.progress_bar_task,
            total=len(self.path_tracker.opened_first_level),
            completed=len(self.path_tracker.closed_first_level),
            open_delta=self.path_tracker.open_delta,
            close_count=self.path_tracker.close_count,
        )

        self.current_path.truncate(0)
        self.current_path.append(
            "\n".join([str(s) for s in self.path_tracker.current_tree])
        )
        for straggler in self.path_tracker.stragglers[self.stragglers.row_count :]:
            self.stragglers.add_row(*list(map(str, straggler)))

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

    def current_path(self, path: Path) -> None:
        """Event for recursing into path"""
        self.path_tracker.current_path(path)

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        self.path_tracker.open_paths(paths)

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.path_tracker.close_path(path)


class VoidStatusTracker:
    """Provides tracker interface, outputs nothing"""

    def __init__(self, path: Path):
        pass

    def current_path(self, path: Path) -> None:
        """Event for recursing into path"""

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""

    def close_path(self, path: Path) -> None:
        """Prints current status"""

    def __enter__(self) -> Self:
        return self

    def __exit__(self, exc_type: None, exc_val: None, exc_tb: None) -> None:
        """Notify the status tracker, that crawling has stopped"""


StatusTracker = TimingStatusTracker | VoidStatusTracker
