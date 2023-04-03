"""Contains StatusTracker class"""
import time
from pathlib import Path

import rich.console
import rich.live
import rich.markup
import rich.text
from typing_extensions import Self


class TimingStatusTracker:
    """Trackes status of crawling"""

    def __init__(self, root: Path, console: rich.console.Console):
        self.live_display = rich.live.Live(None, console=console)
        self.live_display.__enter__()
        self.root = root
        self.open_count = 0
        self.close_count = 0
        self.start_time = time.time()
        self.last_status_time = self.start_time
        self.last_opened: str | None = None

    def __enter__(self) -> Self:
        return self

    def __exit__(self, exc_type: None, exc_val: None, exc_tb: None) -> None:
        """Notify the status tracker, that crawling has stopped"""
        self.live_display.__exit__(*([None] * 3))

    def open_paths(self, paths: list[Path]) -> None:
        """Event to open paths"""
        self.open_count += len(paths)
        if paths:
            self.last_opened = str(paths[-1])
        self._print_status()

    def close_path(self, _path: Path) -> None:
        """Event to close path"""
        self.close_count += 1
        self._print_status()

    @property
    def runtime(self) -> float:
        """Returns the time the tracker has been running"""
        return self.last_status_time - self.start_time

    def _print_status(self) -> None:
        """Prints current status"""
        if time.time() - self.last_status_time < 0.1:
            return
        self.last_status_time = time.time()

        status_text = rich.text.Text()
        status_text.append(f"{self.runtime:>7.2f}", style="bold")
        status_text.append(" ")
        status_text.append(f"{self.open_count - self.close_count:>4}", style="#ADD8E6")
        status_text.append(" + ")
        status_text.append(f"{self.close_count:>6}", style="#808080")
        status_text.append("\n\t")
        status_text.append(rich.markup.escape(str(self.last_opened)))

        self.live_display.update(
            status_text,
            refresh=True,
        )


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
