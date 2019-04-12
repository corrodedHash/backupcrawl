"""Pacman check"""
import subprocess
import enum
from typing import List, Tuple, Optional, Dict
from pathlib import Path


class PacmanSyncStatus(enum.Enum):
    """Different sync states for a pacman controlled file"""
    NOPAC = enum.auto()
    CHANGED = enum.auto()
    CLEAN = enum.auto()


FILE_DICT: Optional[Dict[str, str]] = None


def _initialize_dict() -> None:
    pacman_output = subprocess.run(
        ["pacman", "-Ql"], stdout=subprocess.PIPE)
    global FILE_DICT
    FILE_DICT = {
        path: package for package, path in (
            l.split(maxsplit=1) for l in (
                l for l in pacman_output.stdout.decode('utf-8').splitlines()))}


def check_file(filename: Path) -> Optional[str]:
    """Checks if a single file is managed by pacman, returns the package"""
    if not FILE_DICT:
        _initialize_dict()
    try:
        return FILE_DICT[str(filename)]
    except KeyError:
        return None


def pacman_check(path: str) -> PacmanSyncStatus:
    """Check if a pacman controlled file is clean"""
    pacman_status = subprocess.run(
        ["pacfile", "--check", path], stdout=subprocess.PIPE)

    pacman_output = pacman_status.stdout.decode('utf-8')

    if pacman_output.startswith("no package owns"):
        return PacmanSyncStatus.NOPAC

    assert pacman_output.startswith("file:")

    for pacman_line in pacman_output.splitlines():
        if pacman_line.startswith("sha256:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED
        elif pacman_line.startswith("md5sum:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED

    return PacmanSyncStatus.CLEAN
