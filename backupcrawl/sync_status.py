"""Contains SyncStatus class"""
import enum
from dataclasses import dataclass
from pathlib import Path


class SyncStatus(enum.Enum):
    """Different backup states"""

    NONE = enum.auto()
    CLEAN = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


@dataclass
class BackupEntry:
    """Entry for a backup"""

    path: Path
    status: SyncStatus


class DirChecker:
    def check_dir(self, path: Path) -> BackupEntry:
        raise NotImplemented


class FileChecker:
    def check_file(self, path: Path) -> BackupEntry:
        raise NotImplemented