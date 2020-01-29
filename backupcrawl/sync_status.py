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
