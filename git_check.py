"""Git check"""
import subprocess
import enum


class GitSyncStatus(enum.Enum):
    """Different backup states a git repository can be in"""
    NOGIT = enum.auto()
    CLEAN_SYNCED = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


def _git_check_ahead(path: str) -> bool:
    """Checks if a git repository got a branch that is ahead of the remote branch"""

    git_for_each = subprocess.run(
        ["git", "for-each-ref",
         "--format='%(upstream:trackshort)'", "refs/heads"],
        cwd=path, stdout=subprocess.PIPE)

    if git_for_each.returncode != 0:
        raise RuntimeError

    return any(x in (b"'>'", b"'<>'", b"''")
               for x in git_for_each.stdout.splitlines())


def git_check(path: str) -> GitSyncStatus:
    """Checks if a git repository is clean"""
    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    if git_status.returncode != 0:
        return GitSyncStatus.NOGIT

    if git_status.stdout != b"":
        return GitSyncStatus.DIRTY

    if _git_check_ahead(path):
        return GitSyncStatus.AHEAD

    return GitSyncStatus.CLEAN_SYNCED
