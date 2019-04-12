"""Contains BackupCrawler class"""

import logging
import dataclasses
from dataclasses import dataclass
from typing import List, Tuple, Optional
from pathlib import Path
import enum
import asyncio
from git_check import GitSyncStatus, git_check_root
from pacman_check import PacmanSyncStatus, pacman_check


@dataclass
class _ScanConfig:
    check_pacman: bool = False
    ignore_paths: List[Path] = dataclasses.field(default_factory=list)


@dataclass
class GitRepo():
    """An entry for the backup scan"""
    path: Path
    git_status: GitSyncStatus = GitSyncStatus.NOGIT
    pacman_status: PacmanSyncStatus = PacmanSyncStatus.NOPAC


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


async def _scan(root: Path,
                config: _ScanConfig) -> Tuple[bool, List[Path], List[GitRepo]]:
    """Iterates depth first looking for git repositories"""
    logging.debug("Entering %s", root)
    result: List[Path] = []
    repo_info: List[GitRepo] = []
    split_tree: bool = False

    for current_file in root.iterdir():

        if current_file in config.ignore_paths:
            continue

        if current_file.is_symlink():
            continue

        if not current_file.is_dir():
            if not current_file.is_file():
                # If path is not a directory, or a file,
                # it is some socket or pipe. We don't care
                continue
            if not config.check_pacman:
                pacman_status = PacmanSyncStatus.NOPAC
            else:
                pacman_status = pacman_check(str(current_file))

            if pacman_status == PacmanSyncStatus.NOPAC:
                result.append(current_file)
                continue

            repo_info.append(
                GitRepo(current_file, pacman_status=pacman_status))
            split_tree = True
            continue

        try:
            (current_file / 'hehehehe').exists()
        except PermissionError:
            print("No permissions for {str(current_file)}")
            continue

        git_status = git_check_root(current_file)
        if git_status != GitSyncStatus.NOGIT:
            repo_info.append(GitRepo(current_file, git_status))
            split_tree = True
            continue

        current_result = await _scan(current_file, config)
        repo_info.extend(current_result[2])
        if current_result[0]:
            result.extend(current_result[1])
            split_tree = True
        elif current_result[1]:
            result.append(current_file)

    return (split_tree, result, repo_info)


def scan(root: Path,
         check_pacman: bool = False,
         ignore_paths: Optional[List[Path]] = None) -> Tuple[bool,
                                                             List[Path],
                                                             List[GitRepo]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []
    return asyncio.run(
        _scan(
            root,
            _ScanConfig(
                check_pacman=check_pacman,
                ignore_paths=ignore_paths)))
