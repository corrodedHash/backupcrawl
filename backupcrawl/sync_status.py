"""Contains SyncStatus class"""
import enum
from dataclasses import dataclass
from pathlib import Path
import abc


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

    @staticmethod
    def name() -> str:
        """Display name of the backup entry type"""
        return "Generic path"


class DirChecker(abc.ABC):
    """Abstract base class for checking the backup status of a directory"""

    @abc.abstractmethod
    def check_dir(self, path: Path) -> BackupEntry:
        """Check if directory is backed up"""
        raise NotImplementedError()


class FileChecker(abc.ABC):
    """Abstract base class for checking the backup status of a file"""

    @abc.abstractmethod
    def check_file(self, filepath: Path) -> BackupEntry:
        """Check if file is backed up"""
        raise NotImplementedError()
