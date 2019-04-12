"""Git check"""
import subprocess
import enum
from pathlib import Path


class GitSyncStatus(enum.Enum):
    """Different backup states a git repository can be in"""
    NOGIT = enum.auto()
    CLEAN_SYNCED = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


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


def git_check_root(path: Path) -> GitSyncStatus:
    """Checks if a git repository is clean"""
    if not (path / '.git').is_dir():
        return GitSyncStatus.NOGIT

    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    assert git_status.returncode == 0

    if git_status.stdout != b"":
        return GitSyncStatus.DIRTY

    if _git_check_ahead(path):
        return GitSyncStatus.AHEAD

    return GitSyncStatus.CLEAN_SYNCED
