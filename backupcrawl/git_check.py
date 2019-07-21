"""Git check"""
import enum
from pathlib import Path
import logging
import asyncio
from dataclasses import dataclass
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


async def _git_check_ahead(path: Path, semaphore: asyncio.Semaphore) -> bool:
    """Checks if a git repository got a branch that
    is ahead of the remote branch"""

    async with semaphore:
        git_process = await asyncio.create_subprocess_shell(
            "git for-each-ref --format='%(upstream:trackshort)' refs/heads",
            cwd=path,
            stdout=asyncio.subprocess.PIPE)

        git_bytes_stdout, _ = await git_process.communicate()

    if git_process.returncode != 0:
        raise RuntimeError

    return any(x in (b"'>'", b"'<>'", b"''")
               for x in git_bytes_stdout.splitlines())


async def git_check_root(path: Path, semaphore: asyncio.Semaphore) -> GitRepo:
    """Checks if a git repository is clean"""
    if not (path / '.git').is_dir():
        return GitRepo(path=path, status=GitSyncStatus.NOGIT)

    async with semaphore:
        git_process = await asyncio.create_subprocess_shell(
            "git status --porcelain",
            cwd=path,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL)

        git_bytes_stdout, _ = await git_process.communicate()

    MODULE_LOGGER.debug("Calling git shell command at %s", str(path))

    assert git_process.returncode == 0

    if git_bytes_stdout != b"":
        return GitRepo(path=path, status=GitSyncStatus.DIRTY)

    if await _git_check_ahead(path, semaphore):
        return GitRepo(path=path, status=GitSyncStatus.AHEAD)

    return GitRepo(path=path, status=GitSyncStatus.CLEAN_SYNCED)
