"""Pacman check"""
import logging
import subprocess
from pathlib import Path
from typing import Dict
from dataclasses import dataclass

from .sync_status import SyncStatus, BackupEntry

MODULE_LOGGER = logging.getLogger("backupcrawl.pacman_check")


@dataclass
class PacmanBackupEntry(BackupEntry):  # pylint: disable=R0903
    """An entry for a file managed by pacman"""

    package: str = ""


def _pacman_differs(filepath: Path) -> SyncStatus:
    """Check if a pacman controlled file is clean"""
    pacman_process = subprocess.run(
        ["pacfile", "--check", f"{filepath}"],
        text=True,
        capture_output=True,
        check=True,
    )

    assert pacman_process.returncode == 0
    assert not pacman_process.stdout.startswith("no package owns")
    assert pacman_process.stdout.startswith("file:")

    for pacman_line in pacman_process.stdout.splitlines():
        if pacman_line.startswith("sha256:"):
            if pacman_line.endswith("on filesystem)"):
                return SyncStatus.DIRTY
        elif pacman_line.startswith("md5sum:"):
            if pacman_line.endswith("on filesystem)"):
                return SyncStatus.DIRTY

    return SyncStatus.CLEAN


def _initialize_dict() -> Dict[str, str]:
    pacman_process = subprocess.run(["pacman", "-Ql"], capture_output=True, text=True, check=True)

    result = {
        path: package
        for package, path in (
            l.split(maxsplit=1) for l in pacman_process.stdout.splitlines()
        )
    }
    return result


_FILE_DICT: Dict[str, str] = _initialize_dict()


def is_pacman_file(filepath: Path) -> PacmanBackupEntry:
    """Checks if a single file is managed by pacman, returns the package"""

    global _FILE_DICT

    try:
        pacman_pkg = _FILE_DICT[str(filepath)]
    except KeyError:
        return PacmanBackupEntry(path=filepath, status=SyncStatus.NONE)
    MODULE_LOGGER.debug("Calling pacfile command on %s", str(filepath))

    return PacmanBackupEntry(
        path=filepath, status=_pacman_differs(filepath), package=pacman_pkg
    )
