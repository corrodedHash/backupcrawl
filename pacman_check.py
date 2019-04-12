"""Pacman check"""
import subprocess
import enum
from typing import List, Optional, Dict
from pathlib import Path
import functools


class PacmanSyncStatus(enum.Enum):
    """Different sync states for a pacman controlled file"""
    NOPAC = enum.auto()
    CHANGED = enum.auto()
    CLEAN = enum.auto()


def check_file(filename: Path) -> Optional[str]:
    """Checks if a single file is managed by pacman, returns the package"""
    def _initialize_dict() -> Dict[str, str]:
        pacman_output = subprocess.run(
            ["pacman", "-Ql"], stdout=subprocess.PIPE)
        result = {
            path: package for package, path in (
                l.split(maxsplit=1) for l in (
                    l for l in pacman_output.stdout.decode('utf-8')
                    .splitlines()))}
        return result
    if not check_file.file_dict:
        check_file.file_dict = _initialize_dict()
        assert check_file.file_dict
    try:
        return check_file.file_dict[str(filename)]
    except KeyError:
        return None


setattr(check_file, 'file_dict', None)


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
