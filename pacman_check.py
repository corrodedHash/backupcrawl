"""Pacman check"""
import subprocess
import enum
from typing import Dict
from pathlib import Path
from dataclasses import dataclass
import tarfile
import logging
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


def _pacman_differs(filepath: Path) -> PacmanSyncStatus:
    """Check if a pacman controlled file is clean"""
    pacman_status = subprocess.run(
        ["pacfile", "--check", str(filepath)], stdout=subprocess.PIPE)

    pacman_output = pacman_status.stdout.decode('utf-8')

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


def is_pacman_file(filepath: Path) -> PacmanFile:
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

    if not is_pacman_file.file_dict:
        is_pacman_file.file_dict = _initialize_dict()
        assert is_pacman_file.file_dict
    try:
        pacman_pkg = is_pacman_file.file_dict[str(filepath)]
    except KeyError:
        return PacmanFile(path=filepath, status=PacmanSyncStatus.NOPAC)

    return PacmanFile(
        path=filepath,
        status=_pacman_differs(filepath),
        package=pacman_pkg)


setattr(is_pacman_file, 'file_dict', None)
