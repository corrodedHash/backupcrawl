"""Git check"""
import logging
import subprocess
from pathlib import Path

from .sync_status import BackupEntry, DirChecker, SyncStatus

MODULE_LOGGER = logging.getLogger("backupcrawl.git_check")


class GitBackupEntry(BackupEntry):  # pylint: disable=R0903
    """An entry for the backup scan"""

    @staticmethod
    def name() -> str:
        """Display name of the backup entry type"""
        return "Git"


class GitDirChecker(DirChecker):
    """Check if directory is a git repository"""

    def _git_check_ahead(self, path: Path) -> bool:
        """Checks if a git repository got a branch that
        is ahead of the remote branch"""

        git_process = subprocess.run(
            ["git", "for-each-ref", "--format='%(upstream:trackshort)'", "refs/heads"],
            cwd=path,
            capture_output=True,
            check=True,
        )

        if git_process.returncode != 0:
            raise RuntimeError()

        return any(
            x in (b"'>'", b"'<>'", b"''") for x in git_process.stdout.splitlines()
        )

    @staticmethod
    def name() -> str:
        return "Git"

    def check_dir(self, path: Path) -> GitBackupEntry:
        """Checks if a git repository is clean"""
        if not (path / ".git").is_dir():
            return GitBackupEntry(path=path, status=SyncStatus.NONE)

        MODULE_LOGGER.debug("Calling git shell command at %s", str(path))
        try:
            git_process = subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=path,
                capture_output=True,
                check=True,
            )
        except subprocess.CalledProcessError as return_code_error:
            MODULE_LOGGER.warning(
                "`git status --porcelain` returned with error code %s",
                return_code_error,
            )
            return GitBackupEntry(path=path, status=SyncStatus.NONE)

        if git_process.stdout != b"":
            return GitBackupEntry(path=path, status=SyncStatus.DIRTY)

        if self._git_check_ahead(path):
            return GitBackupEntry(path=path, status=SyncStatus.AHEAD)

        return GitBackupEntry(path=path, status=SyncStatus.CLEAN)
