"""Pacman check"""
import subprocess
import enum


class PacmanSyncStatus(enum.Enum):
    """Different sync states for a pacman controlled file"""
    NOPAC = enum.auto()
    CHANGED = enum.auto()
    CLEAN = enum.auto()


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
