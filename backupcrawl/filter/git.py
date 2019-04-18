"""Git check"""
import subprocess
import enum
from pathlib import Path
import logging
from dataclasses import dataclass
from typing import List
from .base import FilterResult
MODULE_LOGGER = logging.getLogger('backupcrawl.git_check')


class GitSyncStatus(enum.Enum):
    """Different backup states a git repository can be in"""
    NOGIT = enum.auto()
    CLEAN_SYNCED = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


@dataclass
class GitRepo():
    """An entry for the backup scan"""
    path: Path
    status: GitSyncStatus = GitSyncStatus.NOGIT


def _git_check_ahead(path: Path) -> bool:
    """Checks if a git repository got a branch that
    is ahead of the remote branch"""

    git_for_each = subprocess.run(
        ["git", "for-each-ref",
         "--format='%(upstream:trackshort)'", "refs/heads"],
        cwd=path, stdout=subprocess.PIPE)

    if git_for_each.returncode != 0:
        raise RuntimeError

    return any(x in (b"'>'", b"'<>'", b"''")
               for x in git_for_each.stdout.splitlines())


def git_check_root(path: Path) -> GitRepo:
    """Checks if a git repository is clean"""
    if not (path / '.git').is_dir():
        return GitRepo(path=path, status=GitSyncStatus.NOGIT)

    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    MODULE_LOGGER.debug("Calling git shell command at %s", str(path))

    assert git_status.returncode == 0

    if git_status.stdout != b"":
        return GitRepo(path=path, status=GitSyncStatus.DIRTY)

    if _git_check_ahead(path):
        return GitRepo(path=path, status=GitSyncStatus.AHEAD)

    return GitRepo(path=path, status=GitSyncStatus.CLEAN_SYNCED)


class GitRootFilter:
    clean_repos: List[Path]
    dirty_repos: List[Path]
    unsynced_repos: List[Path]

    def __init__(self) -> None:
        self.clean_repos = []
        self.dirty_repos = []
        self.unsynced_repos = []

    def __call__(self, current_file: Path) -> FilterResult:

        if not current_file.is_dir():
            return(False, False)

        result = _git_check_ahead(current_file)
        if result == GitSyncStatus.NOGIT:
            return (False, False)
        if result == GitSyncStatus.DIRTY:
            self.dirty_repos.append(current_file)
        if result == GitSyncStatus.AHEAD:
            self.unsynced_repos.append(current_file)
        if result == GitSyncStatus.CLEAN_SYNCED:
            self.clean_repos.append(current_file)

        return (True, True)
