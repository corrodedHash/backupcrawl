"""Pacman check"""
import enum
import logging
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Dict

MODULE_LOGGER = logging.getLogger("backupcrawl.pacman_check")


class PacmanSyncStatus(enum.Enum):
    """Different sync states for a pacman controlled file"""

    NOPAC = enum.auto()
    CHANGED = enum.auto()
    CLEAN = enum.auto()


@dataclass
class PacmanFile:
    """An entry for a file managed by pacman"""

    path: Path
    status: PacmanSyncStatus
    package: str = ""


def _pacman_differs(filepath: Path) -> PacmanSyncStatus:
    """Check if a pacman controlled file is clean"""
    pacman_process = subprocess.run(
        ["pacfile", f"--check {filepath}"], text=True, capture_output=True
    )

    if pacman_process.stdout.startswith("no package owns"):
        raise AssertionError

    assert pacman_process.stdout.startswith("file:")

    for pacman_line in pacman_process.stdout.splitlines():
        if pacman_line.startswith("sha256:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED
        elif pacman_line.startswith("md5sum:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED

    return PacmanSyncStatus.CLEAN


def _initialize_dict() -> Dict[str, str]:
    pacman_process = subprocess.run(["pacman", "-Ql"], capture_output=True, text=True)

    result = {
        path: package
        for package, path in (
            l.split(maxsplit=1) for l in pacman_process.stdout.splitlines()
        )
    }
    return result


_FILE_DICT: Dict[str, str] = _initialize_dict()


def is_pacman_file(filepath: Path) -> PacmanFile:
    """Checks if a single file is managed by pacman, returns the package"""

    global _FILE_DICT

    try:
        pacman_pkg = _FILE_DICT[str(filepath)]
    except KeyError:
        return PacmanFile(path=filepath, status=PacmanSyncStatus.NOPAC)
    MODULE_LOGGER.debug("Calling pacfile command on %s", str(filepath))

    return PacmanFile(
        path=filepath, status=_pacman_differs(filepath), package=pacman_pkg
    )
