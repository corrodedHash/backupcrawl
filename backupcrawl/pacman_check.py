"""Pacman check"""
import subprocess
import enum
from typing import Dict, Optional
from pathlib import Path
from dataclasses import dataclass
import tarfile
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


def _pacman_differs_slow(filepath: Path, package: str) -> PacmanSyncStatus:
    MODULE_LOGGER.debug("Calculating for %s in %s", filepath, package)
    package_archive_names = list(Path(
        '/var/cache/pacman/pkg').glob(package + "*.pkg.tar.xz"))
    assert package_archive_names
    package_archive_names.sort()
    package_name = package_archive_names[-1]

    package_archive = tarfile.open(
        name=Path('/var/cache/pacman/pkg') / package_name, mode="r:xz")
    package_entry_info = package_archive.getmember(str(filepath).strip('/'))
    package_entry_stream = package_archive.extractfile(package_entry_info)
    assert package_entry_stream
    with open(filepath, mode='rb') as real_path_stream:
        BLOCKSIZE = 1024 * 16
        package_block = package_entry_stream.read(BLOCKSIZE)
        real_block = real_path_stream.read(BLOCKSIZE)
        while real_block:
            MODULE_LOGGER.debug("Round tick")
            if package_block != real_block:
                return PacmanSyncStatus.CHANGED
            package_block = package_entry_stream.read(BLOCKSIZE)
            real_block = real_path_stream.read(BLOCKSIZE)

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

    return PacmanFile(
        path=filepath,
        status=await _pacman_differs(filepath),
        package=pacman_pkg)
