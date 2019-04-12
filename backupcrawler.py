"""Contains BackupCrawler class"""

import logging
from typing import List, Tuple, Optional
from pathlib import Path
import enum
import asyncio
from git_check import GitSyncStatus, git_check_root, GitRepo
from pacman_check import PacmanSyncStatus, PacmanFile, is_pacman_file

MODULE_LOGGER = logging.getLogger("backupcrawler")


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


async def _git_crawl(root: Path,
                     ignore_paths: List[Path]) \
        -> Tuple[bool, List[Path], List[GitRepo], List[PacmanFile]]:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result: List[Path] = []
    repo_info: List[GitRepo] = []
    split_tree: bool = False
    pacman_files: List[PacmanFile] = []

    for current_file in root.iterdir():

        if current_file in ignore_paths:
            split_tree = True
            continue

        if current_file.is_symlink():
            continue

        if current_file.is_file():
            pac_result = is_pacman_file(current_file)
            if pac_result.status == PacmanSyncStatus.NOPAC:
                result.append(current_file)
                continue
            pacman_files.append(pac_result)
            split_tree = True
            continue

        if not current_file.is_dir():
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
            continue

        try:
            (current_file / 'hehehehe').exists()
        except PermissionError:
            print(f"No permissions for {str(current_file)}")
            continue

        git_status = git_check_root(current_file)
        if git_status.status != GitSyncStatus.NOGIT:
            repo_info.append(git_status)
            split_tree = True
            continue

        current_result = await _git_crawl(current_file, ignore_paths)
        repo_info.extend(current_result[2])
        pacman_files.extend(current_result[3])
        if current_result[0]:
            result.extend(current_result[1])
            split_tree = True
        elif current_result[1]:
            result.append(current_file)

    return (split_tree, result, repo_info, pacman_files)


def scan(root: Path,
         ignore_paths: Optional[List[Path]] = None) \
        -> Tuple[List[Path], List[GitRepo], List[PacmanFile]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    _, paths, repos, pacman_files = asyncio.run(
        _git_crawl(root, ignore_paths=ignore_paths))

    return (paths, repos, pacman_files)
