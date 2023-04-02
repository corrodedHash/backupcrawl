"""Contains CrawlResult class"""
from collections import defaultdict
from pathlib import Path
from typing import DefaultDict, List

from typing_extensions import Self

from .sync_status import BackupEntry


class CrawlResult:
    """Result from crawl of a single directory"""

    def __init__(self) -> None:
        self.loose_paths: List[Path] = list()
        self.denied_paths: List[Path] = list()
        self.backups: DefaultDict[type, List[BackupEntry]] = defaultdict(list)
        self.path: Path

    def add_backup(self, backup: BackupEntry) -> None:
        """Add backup entry to result"""
        self.backups[type(backup)].append(backup)

    def extend(self, other: Self) -> None:
        """Extend current object with another crawl result"""

        for backup_type in other.backups:
            self.backups[backup_type].extend(other.backups[backup_type])

        self.denied_paths.extend(other.denied_paths)
        if other.backups:
            self.loose_paths.extend(other.loose_paths)
        elif other.loose_paths:
            self.loose_paths.append(other.path)
