"""Contains StatusTracker class"""
from pathlib import Path
from typing import List
import time


class StatusTracker:
    """Trackes status of crawling"""

    def __init__(self, root: Path):
        self.root = root
        self.open_count = 0
        self.close_count = 0
        self.start_time = time.time()
        self.last_status_time = self.start_time
        self.last_close_time = self.start_time
        self.max_close_duration = 0

    def open_paths(self, paths: List[Path]) -> None:
        """Event to open paths"""
        self.open_count += len(paths)

    def close_path(self, path: Path) -> None:
        """Event to close path"""
        self.close_count += 1
        enter_time = time.time()
        close_time = enter_time - self.last_close_time
        if close_time > self.max_close_duration:
            self.max_close_duration = close_time
            print(f"{close_time:>7.2f}: {path}")
        self.last_close_time = enter_time
        if enter_time - self.last_status_time > 2:
            self.print_status()

    def print_status(self) -> None:
        """Prints current status"""
        self.last_status_time = time.time()
        print(
            f"{self.last_status_time - self.start_time:>7.2f} "
            f"{self.close_count:>6} / {self.open_count:>6}"
        )
