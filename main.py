"""Main file"""
from typing import Iterator, List
from dataclasses import dataclass
import os
import subprocess
import enum


class GitSyncStatus(enum.Enum):
    """Different backup states a git repository can be in"""
    NOGIT = enum.auto()
    CLEAN_SYNCED = enum.auto()
    DIRTY = enum.auto()
    AHEAD = enum.auto()


def git_check_ahead(path: str) -> bool:
    """Checks if a git repository got a branch that is ahead of the remote branch"""

    git_for_each = subprocess.run(
        ["git", "for-each-ref",
         "--format='%(upstream:trackshort)'", "refs/heads"],
        cwd=path, stdout=subprocess.PIPE)

    if git_for_each.returncode != 0:
        raise RuntimeError

    return any(x in (b"'>'", b"'<>'", b"''") for x in git_for_each.stdout.splitlines())


def git_check(path: str) -> GitSyncStatus:
    """Checks if a git repository is clean"""
    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    if git_status.returncode != 0:
        return GitSyncStatus.NOGIT

    if git_status.stdout != b"":
        return GitSyncStatus.DIRTY

    if git_check_ahead(path):
        return GitSyncStatus.AHEAD

    return GitSyncStatus.CLEAN_SYNCED


@dataclass
class BackupDir():
    """An entry for the backup scan"""
    path: str
    git_status: GitSyncStatus = GitSyncStatus.NOGIT


def breadth_first_file_scan(root: str) -> Iterator[BackupDir]:
    """ Iterates directory tree breadth first"""
    if git_check(root) != GitSyncStatus.NOGIT:
        yield BackupDir(root, git_check(root))
        return

    dirs = [root]
    while True:
        next_dirs: List[str] = []
        files: List[BackupDir] = []
        split_tree = False
        tree_empty = True
        for parent in dirs:
            for current_file in os.listdir(parent):
                current_file_path = os.path.join(parent, current_file)

                if os.path.islink(current_file_path):
                    continue

                if not os.path.isdir(current_file_path):
                    if not os.path.isfile(current_file_path):
                        # If path is not a directory, or a file,
                        # it is some symlink, socket or pipe. We don't care
                        continue
                    files.append(BackupDir(current_file_path))
                    tree_empty = False
                    continue

                git_status = git_check(current_file_path)
                if git_status != GitSyncStatus.NOGIT:
                    files.append(
                        BackupDir(current_file_path, git_status=git_status))
                    split_tree = True
                    continue

                next_dirs.append(current_file_path)

        if split_tree:
            for directory in next_dirs:
                yield from breadth_first_file_scan(directory)
            yield from files
            return

        if not next_dirs:
            if not tree_empty:
                yield BackupDir(root)
            return

        dirs = next_dirs


def walkbf(path: str) -> None:
    """Main function"""
    clean_repos: List[str] = []
    dirty_repos: List[str] = []
    unsynced_repos: List[str] = []
    for current_file in breadth_first_file_scan(path):
        if current_file.git_status == GitSyncStatus.NOGIT:
            print("\t" + current_file.path)
        elif current_file.git_status == GitSyncStatus.CLEAN_SYNCED:
            clean_repos.append(current_file.path)
        elif current_file.git_status == GitSyncStatus.AHEAD:
            unsynced_repos.append(current_file.path)
        elif current_file.git_status == GitSyncStatus.DIRTY:
            dirty_repos.append(current_file.path)
        else:
            raise RuntimeError

    print("Dirty repositories")
    for repo in dirty_repos:
        print("\t" + repo)
    print("Unsynced repositories")
    for repo in unsynced_repos:
        print("\t" + repo)
    print("Clean repositories")
    for repo in clean_repos:
        print("\t" + repo)


walkbf('/home/lukas')
