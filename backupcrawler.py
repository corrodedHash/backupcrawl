"""Contains BackupCrawler class"""

import logging
import dataclasses
from dataclasses import dataclass
from typing import List, Tuple, Optional
import enum
import os
import asyncio
from git_check import GitSyncStatus, git_check
from pacman_check import PacmanSyncStatus, pacman_check


@dataclass
class _ScanConfig:
    check_pacman: bool = False
    ignore_paths: List[str] = dataclasses.field(default_factory=list)


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


async def _scan(root: str,
                config: _ScanConfig) -> Tuple[bool, List[str], List[GitRepo]]:
    """Iterates depth first looking for git repositories"""
    logging.debug("Entering %s", root)
    result: List[str] = []
    repo_info: List[GitRepo] = []
    split_tree: bool = False

    for current_file in os.listdir(root):
        current_file_path = os.path.join(root, current_file)

        if not os.access(current_file_path, os.R_OK):
            print("No permission for " + current_file_path)
            continue

        if current_file_path in config.ignore_paths:
            continue

        if os.path.islink(current_file_path):
            continue

        if not os.path.isdir(current_file_path):
            if not os.path.isfile(current_file_path):
                # If path is not a directory, or a file,
                # it is some socket or pipe. We don't care
                continue
            if not config.check_pacman:
                pacman_status = PacmanSyncStatus.NOPAC
            else:
                pacman_status = pacman_check(current_file_path)

            if pacman_status == PacmanSyncStatus.NOPAC:
                result.append(current_file_path)
                continue

            repo_info.append(
                GitRepo(current_file_path, pacman_status=pacman_status))
            split_tree = True
            continue

        git_status = git_check(current_file_path)
        if git_status != GitSyncStatus.NOGIT:
            repo_info.append(GitRepo(current_file_path, git_status))
            split_tree = True
            continue

        current_result = await _scan(current_file_path, config)
        repo_info.extend(current_result[2])
        if current_result[0]:
            result.extend(current_result[1])
            split_tree = True
        elif current_result[1]:
            result.append(current_file_path)

    return (split_tree, result, repo_info)


def scan(root: str,
         check_pacman: bool = False,
         ignore_paths: Optional[List[str]] = None) -> Tuple[bool,
                                                            List[str],
                                                            List[GitRepo]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []
    return asyncio.run(_scan(root, _ScanConfig(check_pacman=check_pacman,
                                               ignore_paths=ignore_paths)))
