"""Pacman check"""
import subprocess
import enum
from typing import Dict, List, Tuple
from pathlib import Path
from dataclasses import dataclass
import tarfile
import logging
from .base import FilterResult
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
        blocksize = 1024 * 16
        package_block = package_entry_stream.read(blocksize)
        real_block = real_path_stream.read(blocksize)
        while real_block:
            MODULE_LOGGER.debug("Round tick")
            if package_block != real_block:
                return PacmanSyncStatus.CHANGED
            package_block = package_entry_stream.read(blocksize)
            real_block = real_path_stream.read(blocksize)

    return PacmanSyncStatus.CLEAN


class PacmanFilter:
    file_dict: Dict[str, str]
    clean_files: List[Tuple[Path, str]]
    changed_files: List[Tuple[Path, str]]

    def __init__(self) -> None:
        self.file_dict = self._initialize_dict()
        self.clean_files = []
        self.changed_files = []

    @staticmethod
    def _initialize_dict() -> Dict[str, str]:
        pacman_output = subprocess.run(
            ["pacman", "-Ql"], stdout=subprocess.PIPE)
        result = {
            path: package for package, path in (
                l.split(maxsplit=1) for l in (
                    l for l in pacman_output.stdout.decode('utf-8')
                    .splitlines()))}
        return result

    def __call__(self, current_file: Path) -> FilterResult:
        """Checks if a single file is managed by pacman, returns the package"""

        if not current_file.is_file():
            return (False, False)

        try:
            pacman_pkg = self.file_dict[str(current_file)]
        except KeyError:
            return (False, False)

        status = _pacman_differs(current_file)

        if status == PacmanSyncStatus.CHANGED:
            self.changed_files.append((current_file, pacman_pkg))
        elif status == PacmanSyncStatus.CLEAN:
            self.clean_files.append((current_file, pacman_pkg))
        return (True, True)
