"""Main file"""
from typing import List, Tuple
from dataclasses import dataclass
import os
import enum
from git_check import GitSyncStatus, git_check
from pacman_check import PacmanSyncStatus, pacman_check


@dataclass
class GitRepo():
    """An entry for the backup scan"""
    path: str
    git_status: GitSyncStatus = GitSyncStatus.NOGIT
    pacman_status: PacmanSyncStatus = PacmanSyncStatus.NOPAC


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


def depth_first_file_scan(root: str) -> Tuple[FileScanResult, List[str], List[GitRepo]]:
    """Iterates depth first looking for git repositories"""
    result: List[str] = []
    repo_info: List[GitRepo] = []
    got_repo: FileScanResult = FileScanResult.NO_VC

    for current_file in os.listdir(root):
        current_file_path = os.path.join(root, current_file)

        if os.path.islink(current_file_path):
            continue

        if not os.path.isdir(current_file_path):
            if not os.path.isfile(current_file_path):
                # If path is not a directory, or a file,
                # it is some symlink, socket or pipe. We don't care
                continue
            pacman_status = pacman_check(current_file_path)
            if pacman_status == PacmanSyncStatus.NOPAC:
                result.append(current_file_path)
                continue

            repo_info.append(GitRepo(current_file_path, pacman_status=pacman_status))
            continue

        git_status = git_check(current_file_path)
        if git_status != GitSyncStatus.NOGIT:
            repo_info.append(GitRepo(current_file_path, git_status))
            got_repo = FileScanResult.VC
            continue

        current_result = depth_first_file_scan(current_file_path)
        repo_info.extend(current_result[2])
        if current_result[0] == FileScanResult.VC:
            result.extend(current_result[1])
            got_repo = FileScanResult.VC
        elif current_result[1]:
            result.append(current_file_path)

    return (got_repo, result, repo_info)


def walkbf(path: str) -> None:
    """Main function"""
    sync_tree = depth_first_file_scan(path)
    for current_file in sync_tree[1]:
        print("\t" + current_file)

    for enum_state, status_string in (
            (GitSyncStatus.DIRTY, "Dirty repositories"),
            (GitSyncStatus.AHEAD, "Unsynced repositories"),
            (GitSyncStatus.CLEAN_SYNCED, "Clean repositories")):
        print(status_string + ":")
        for current_file in [t.path for t in sync_tree[2] if t.git_status == enum_state]:
            print("\t" + current_file)


walkbf('/home/lukas')
