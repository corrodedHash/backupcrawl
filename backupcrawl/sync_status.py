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
    """Abstract base class for checking the backup status of a directory"""

    def check_dir(self, path: Path) -> BackupEntry:
        """Check if directory is backed up"""
        raise NotImplementedError()


class FileChecker:
    """Abstract base class for checking the backup status of a file"""

    def check_file(self, filepath: Path) -> BackupEntry:
        """Check if file is backed up"""
        raise NotImplementedError()
