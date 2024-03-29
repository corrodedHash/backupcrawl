"""Pacman check"""
import itertools
import logging
import subprocess
from dataclasses import dataclass
from pathlib import Path

from .sync_status import BackupEntry, FileChecker, SyncStatus

MODULE_LOGGER = logging.getLogger("backupcrawl.pacman_check")


@dataclass
class PacmanBackupEntry(BackupEntry):  # pylint: disable=R0903
    """An entry for a file managed by pacman"""

    package: str = ""

    @staticmethod
    def name() -> str:
        """Display name of the backup entry type"""
        return "Pacman"


class PacmanFileChecker(FileChecker):
    """Check if given path is installed with pacman"""

    def __init__(self) -> None:
        self._file_dict: dict[str, str] | None = None
        self._dirty_file_dict: dict[str, str] | None = None
        self.unknown_reasons: list[str] = []

    def _pacman_differs(self, filepath: Path) -> SyncStatus:
        """Check if a pacman controlled file is clean"""
        if self._dirty_file_dict is None:
            MODULE_LOGGER.debug("Initializing dirty pacman files")
            self._dirty_file_dict = self._get_dirty_pacman_dict()
        if str(filepath) in self._dirty_file_dict:
            return SyncStatus.DIRTY
        return SyncStatus.CLEAN

    def _get_pacman_dict(self) -> dict[str, str]:
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

    def _get_dirty_pacman_dict(self) -> dict[str, str]:
        pacman_process = subprocess.run(
            ["pacman", "-Qkk"], capture_output=True, text=True, check=False
        )
        lines = (
            line
            for line in itertools.chain(
                pacman_process.stdout.splitlines(), pacman_process.stderr.splitlines()
            )
            if line.startswith("warning:") or line.startswith("backup file:")
        )

        def _parse_line(line: str) -> tuple[str, str]:
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
            stripped_reason = reason.strip(")")
            if (
                stripped_reason not in reasons
                and stripped_reason not in self.unknown_reasons
            ):
                self.unknown_reasons.append(stripped_reason)
                MODULE_LOGGER.warning("Unknown reason '%s'", stripped_reason)
            return (package, path)

        parsed_lines = (_parse_line(line) for line in lines)
        result = {path: package for package, path in parsed_lines}
        return result

    def check_file(self, filepath: Path) -> PacmanBackupEntry:
        """Checks if a single file is managed by pacman, returns the package"""
        if self._file_dict is None:
            self._file_dict = self._get_pacman_dict()
        try:
            pacman_pkg = self._file_dict[str(filepath)]
        except KeyError:
            return PacmanBackupEntry(path=filepath, status=SyncStatus.NONE)
        MODULE_LOGGER.debug("Calling pacfile command on %s", str(filepath))

        return PacmanBackupEntry(
            path=filepath, status=self._pacman_differs(filepath), package=pacman_pkg
        )
