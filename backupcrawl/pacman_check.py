"""Pacman check"""
import logging
import subprocess
from pathlib import Path
from typing import Dict, Tuple
from dataclasses import dataclass
import itertools
from .sync_status import SyncStatus, BackupEntry

MODULE_LOGGER = logging.getLogger("backupcrawl.pacman_check")


@dataclass
class PacmanBackupEntry(BackupEntry):  # pylint: disable=R0903
    """An entry for a file managed by pacman"""

    package: str = ""


def _pacman_differs(filepath: Path) -> SyncStatus:
    """Check if a pacman controlled file is clean"""
    if str(filepath) in _DIRTY_FILE_DICT:
        return SyncStatus.DIRTY
    return SyncStatus.CLEAN


def _get_pacman_dict() -> Dict[str, str]:
    pacman_process = subprocess.run(
        ["pacman", "-Ql"], capture_output=True, text=True, check=True
    )

    result = {
        path: package
        for package, path in (
            l.split(maxsplit=1) for l in pacman_process.stdout.splitlines()
        )
    }
    return result


def _get_dirty_pacman_dict() -> Dict[str, str]:
    pacman_process = subprocess.run(["pacman", "-Qkk"], capture_output=True, text=True)
    lines = (
        line
        for line in itertools.chain(
            pacman_process.stdout.splitlines(), pacman_process.stderr.splitlines()
        )
        if line.startswith("warning:") or line.startswith("backup file:")
    )

    def _parse_line(line: str) -> Tuple[str, str]:
        _, package, colon_rest = line.split(":", maxsplit=2)
        package = package.strip()
        path, reason = colon_rest.split("(", maxsplit=1)
        path = path.strip()
        reasons = [
            "Modification time mismatch",
            "Size mismatch",
            "GID mismatch",
            "Permissions mismatch",
            "UID mismatch",
            "Permission denied",
            "Symlink path mismatch",
            "File type mismatch",
        ]
        if reason.strip(")") not in reasons:
            MODULE_LOGGER.warning("Unknown reason '%s'", reason.strip(")"))
        return (package, path)

    parsed_lines = (_parse_line(line) for line in lines)
    result = {path: package for package, path in parsed_lines}
    return result


_FILE_DICT: Dict[str, str] = _get_pacman_dict()
_DIRTY_FILE_DICT: Dict[str, str] = _get_dirty_pacman_dict()


def is_pacman_file(filepath: Path) -> PacmanBackupEntry:
    """Checks if a single file is managed by pacman, returns the package"""

    try:
        pacman_pkg = _FILE_DICT[str(filepath)]
    except KeyError:
        return PacmanBackupEntry(path=filepath, status=SyncStatus.NONE)
    MODULE_LOGGER.debug("Calling pacfile command on %s", str(filepath))

    return PacmanBackupEntry(
        path=filepath, status=_pacman_differs(filepath), package=pacman_pkg
    )
