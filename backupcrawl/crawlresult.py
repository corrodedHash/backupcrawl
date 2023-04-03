"""Contains CrawlResult class"""
from collections import defaultdict
from pathlib import Path

from typing_extensions import Self

from .sync_status import BackupEntry


class CrawlResult:
    """Result from crawl of a single directory"""

    def __init__(self, path: Path) -> None:
        self.loose_paths: list[Path] = []
        self.denied_paths: list[Path] = []
        self.backups: defaultdict[type[BackupEntry], list[BackupEntry]] = defaultdict(list)
        self.path: Path = path

    def add_backup(self, backup: BackupEntry) -> None:
        """Add backup entry to result"""
        self.backups[type(backup)].append(backup)

    def extend(self, other: Self) -> None:
        """Extend current object with another crawl result"""

        for backup_type in other.backups:
            self.backups[backup_type].extend(other.backups[backup_type])

        self.denied_paths.extend(other.denied_paths)

        # If `other` has no backed up paths, we can just mark the entire tree as not backed up
        if other.backups:
            self.loose_paths.extend(other.loose_paths)
        elif other.loose_paths:
            self.loose_paths.append(other.path)
