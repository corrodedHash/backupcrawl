"""Main file"""
from typing import Iterator, List
from dataclasses import dataclass
import os
import subprocess


def is_vc_protected(path: str) -> bool:
    """Returns whether a file or a directory is version controlled by git"""
    if os.path.isfile(path):
        path = os.path.dirname(os.path.realpath(path))
    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path, stdout=subprocess.PIPE)
    if git_status.returncode == 0:
        return True
    return False


def is_vc_root(path: str) -> bool:
    """Checks if given path is the root of a git repo"""
    return os.path.isdir(os.path.join(path, '.git'))


def git_is_clean(path: str) -> bool:
    """Checks if a git repository is clean"""
    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path, stdout=subprocess.PIPE)

    if git_status.returncode != 0:
        raise RuntimeError

    result: bool = git_status.stdout == b""

    return result


def git_pushed(path: str) -> bool:
    """Checks if a git repository has all local branches pushed"""

    git_for_each = subprocess.run(
        ["git", "for-each-ref", "--format='%(upstream:trackshort)'", "refs/heads"],
        cwd=path, stdout=subprocess.PIPE)

    if git_for_each.returncode != 0:
        raise RuntimeError

    return all(x in (b"'<'", b"'='") for x in git_for_each.stdout.splitlines())


@dataclass
class BackupDir():
    """An entry for the backup scan"""
    path: str
    symlink: bool = False
    is_git_repo: bool = False
    repo_clean: bool = False
    repo_pushed: bool = False


def breadth_first_file_scan(root: str) -> Iterator[BackupDir]:
    """ Iterates directory tree breadth first"""
    dirs = [root]
    while True:
        next_dirs: List[str] = []
        files: List[BackupDir] = []
        got_vc = False
        for parent in dirs:
            for current_file in os.listdir(parent):
                current_file_path = os.path.join(parent, current_file)
                if os.path.islink(current_file_path):
                    files.append(BackupDir(current_file_path, symlink=True))
                    continue

                if not os.path.isdir(current_file_path):
                    files.append(BackupDir(current_file_path))
                    continue

                if is_vc_root(current_file_path):
                    files.append(BackupDir(
                        current_file_path, is_git_repo=True,
                        repo_clean=git_is_clean(current_file_path), repo_pushed=git_pushed(current_file_path)))
                    got_vc = True
                    continue

                next_dirs.append(current_file_path)

        if got_vc:
            for directory in next_dirs:
                yield from breadth_first_file_scan(directory)
            yield from files
            return

        if not next_dirs:
            yield BackupDir(root)
            return

        dirs = next_dirs


def walkbf(path: str) -> None:
    """Main function"""
    clean_repos: List[str] = []
    dirty_repos: List[str] = []
    unsynced_repos: List[str] = []
    symlinks: List[str] = []
    for current_file in breadth_first_file_scan(path):
        if current_file.symlink:
            symlinks.append(current_file.path)
        elif current_file.is_git_repo:
            if current_file.repo_clean:
                clean_repos.append(current_file.path)
            elif not current_file.repo_pushed:
                unsynced_repos.append(current_file.path)
            else:
                dirty_repos.append(current_file.path)
        else:
            print("\t" + current_file.path)
    print("Dirty repositories")
    for repo in dirty_repos:
        print("\t" + repo)
    print("Unsynced repositories")
    for repo in unsynced_repos:
        print("\t" + repo)
    print("Clean repositories")
    for repo in clean_repos:
        print("\t" + repo)
    print("Symlinks")
    for repo in symlinks:
        print("\t" + repo)


walkbf('/home/lukas/')
