"""Main file"""
from typing import Iterator, List
from dataclasses import dataclass
import os


def is_vc_root(path: str) -> bool:
    """Checks if given path is the root of a git repo"""
    return os.path.isdir(os.path.join(path, '.git'))

def git_backedup(path: str) -> bool:
    """Checks if a git repository is pushed and clean"""
    pass

@dataclass
class BackupDir():
    """An entry for the backup scan"""
    path: str
    is_git_repo: bool = False
    git_repo_saved: bool = False



def breadth_first_file_scan(root: str) -> Iterator[str]:
    """ Iterates directory tree breadth first"""
    dirs = [root]
    while True:
        next_dirs: List[str] = []
        files: List[str] = []
        got_vc = False
        for parent in dirs:
            for current_file in os.listdir(parent):
                current_file_path = os.path.join(parent, current_file)
                if not os.path.isdir(current_file_path):
                    files.append(current_file_path)
                    continue
                if is_vc_root(current_file_path):
                    got_vc = True
                    continue
                next_dirs.append(current_file_path)

        if got_vc:
            for directory in next_dirs:
                yield from breadth_first_file_scan(directory)
            yield from files
            return

        if not next_dirs:
            yield root
            return

        dirs = next_dirs


def walkbf(path: str) -> None:
    """Main function"""
    for current_file in breadth_first_file_scan(path):
        print(current_file)


walkbf('/home/lukas/documents/')
