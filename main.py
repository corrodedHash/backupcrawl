"""Main file"""
from typing import Iterator, List
from dataclasses import dataclass
import os
import subprocess


def is_vc_root(path: str) -> bool:
    """Checks if given path is the root of a git repo"""
    return os.path.isdir(os.path.join(path, '.git'))


def git_clean(path: str) -> bool:
    """Checks if a git repository is clean"""
    git_status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=path, stdout=subprocess.PIPE)

    if git_status.returncode != 0:
        raise RuntimeError

    result: bool = git_status.stdout == b""

    return result


def git_pushed(path: str) -> bool:
    """Checks if a git repository has all local branches pushed"""
    return not path


@dataclass
class BackupDir():
    """An entry for the backup scan"""
    path: str
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
                if not os.path.isdir(current_file_path):
                    files.append(BackupDir(current_file_path))
                    continue
                if is_vc_root(current_file_path):
                    files.append(BackupDir(
                        current_file_path, is_git_repo=True,
                        repo_clean=git_clean(current_file_path)))
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
    for current_file in breadth_first_file_scan(path):
        if current_file.is_git_repo:
            if current_file.repo_clean:
                clean_repos.append(current_file.path)
            else:
                dirty_repos.append(current_file.path)
        else:
            print(current_file.path)
    print("Dirty repositories")
    for repo in dirty_repos:
        print(repo)
    print("Clean repositories")
    for repo in clean_repos:
        print(repo)


walkbf('/home/lukas/documents/')
