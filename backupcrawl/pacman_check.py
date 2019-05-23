"""Pacman check"""
import enum
from typing import Dict, Optional
from pathlib import Path
from dataclasses import dataclass
import logging
import asyncio
MODULE_LOGGER = logging.getLogger("backupcrawl.pacman_check")


class PacmanSyncStatus(enum.Enum):
    """Different sync states for a pacman controlled file"""
    NOPAC = enum.auto()
    CHANGED = enum.auto()
    CLEAN = enum.auto()


@dataclass
class PacmanFile():
    """An entry for a file managed by pacman"""
    path: Path
    status: PacmanSyncStatus
    package: str = ""


async def _pacman_differs(filepath: Path) -> PacmanSyncStatus:
    """Check if a pacman controlled file is clean"""
    pacman_process = await asyncio.create_subprocess_shell(
        f"pacfile --check {filepath}",
        stdout=asyncio.subprocess.PIPE)

    pacman_bytes_stdout, _ = await pacman_process.communicate()
    pacman_output = pacman_bytes_stdout.decode()

    if pacman_output.startswith("no package owns"):
        raise AssertionError

    assert pacman_output.startswith("file:")

    for pacman_line in pacman_output.splitlines():
        if pacman_line.startswith("sha256:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED
        elif pacman_line.startswith("md5sum:"):
            if pacman_line.endswith("on filesystem)"):
                return PacmanSyncStatus.CHANGED

    return PacmanSyncStatus.CLEAN


async def _initialize_dict() -> Dict[str, str]:
    pacman_process = await asyncio.create_subprocess_shell(
        "pacman -Ql",
        stdout=asyncio.subprocess.PIPE)

    pacman_bytes_stdout, _ = await pacman_process.communicate()
    pacman_output = pacman_bytes_stdout.decode()

    result = {
        path: package for package, path in (
            l.split(maxsplit=1) for l in (
                l for l in pacman_output
                .splitlines()))}
    return result


_FILE_DICT: Optional[Dict[str, str]] = None


async def is_pacman_file(filepath: Path) -> PacmanFile:
    """Checks if a single file is managed by pacman, returns the package"""

    global _FILE_DICT
    if _FILE_DICT is None:
        _FILE_DICT = await _initialize_dict()
        assert _FILE_DICT is not None
    try:
        pacman_pkg = _FILE_DICT[str(filepath)]
    except KeyError:
        return PacmanFile(path=filepath, status=PacmanSyncStatus.NOPAC)
    MODULE_LOGGER.info("Calling pacfile command on %s", str(filepath))

    return PacmanFile(
        path=filepath,
        status=await _pacman_differs(filepath),
        package=pacman_pkg)
