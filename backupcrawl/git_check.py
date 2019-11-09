"""Git check"""
import enum
import logging
import subprocess
from dataclasses import dataclass
from pathlib import Path

MODULE_LOGGER = logging.getLogger("backupcrawl.git_check")


class GitSyncStatus(enum.Enum):
    """Different backup states a git repository can be in"""

    NOGIT = enum.auto()
    CLEAN_SYNCED = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


@dataclass
class GitRepo:
    """An entry for the backup scan"""

    path: Path
    status: GitSyncStatus = GitSyncStatus.NOGIT


def _git_check_ahead(path: Path) -> bool:
    """Checks if a git repository got a branch that
    is ahead of the remote branch"""

    git_process = subprocess.run(
        ["git", "for-each-ref", "--format='%(upstream:trackshort)'", "refs/heads"],
        cwd=path,
        capture_output=True,
    )

    if git_process.returncode != 0:
        raise RuntimeError

    return any(x in (b"'>'", b"'<>'", b"''") for x in git_process.stdout.splitlines())


def git_check_root(path: Path) -> GitRepo:
    """Checks if a git repository is clean"""
    if not (path / ".git").is_dir():
        return GitRepo(path=path, status=GitSyncStatus.NOGIT)

    git_process = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path, capture_output=True
    )

    MODULE_LOGGER.debug("Calling git shell command at %s", str(path))

    assert git_process.returncode == 0

    if git_process.stdout != b"":
        return GitRepo(path=path, status=GitSyncStatus.DIRTY)

    if _git_check_ahead(path):
        return GitRepo(path=path, status=GitSyncStatus.AHEAD)

    return GitRepo(path=path, status=GitSyncStatus.CLEAN_SYNCED)
