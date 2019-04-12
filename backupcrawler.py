"""Contains BackupCrawler class"""

import logging
import dataclasses
from dataclasses import dataclass
from typing import List, Tuple, Optional
from pathlib import Path
import enum
import asyncio
from git_check import GitSyncStatus, git_check_root
import pacman_check

MODULE_LOGGER = logging.getLogger("backupcrawler")


@dataclass
class _ScanConfig:
    check_pacman: bool = False
    ignore_paths: List[Path] = dataclasses.field(default_factory=list)


@dataclass
class GitRepo():
    """An entry for the backup scan"""
    path: Path
    git_status: GitSyncStatus = GitSyncStatus.NOGIT

@dataclass
class PacmanFile():
    path: Path


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


async def _git_crawl(root: Path,
                     config: _ScanConfig) \
        -> Tuple[bool, List[Path], List[GitRepo], List[PacmanFile]]:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result: List[Path] = []
    repo_info: List[GitRepo] = []
    split_tree: bool = False
    pacman_files: List[PacmanFile] = []

    for current_file in root.iterdir():

        if current_file in config.ignore_paths:
            split_tree = True
            continue

        if current_file.is_symlink():
            continue

        if current_file.is_file():
            pac_result = pacman_check.check_file(current_file)
            if pac_result:
                pacman_files.append(PacmanFile(path=current_file))
                split_tree = True
            else:
                result.append(current_file)
            continue

        if not current_file.is_dir():
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
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

        current_result = await _git_crawl(current_file, config)
        repo_info.extend(current_result[2])
        pacman_files.extend(current_result[3])
        if current_result[0]:
            result.extend(current_result[1])
            split_tree = True
        elif current_result[1]:
            result.append(current_file)

    return (split_tree, result, repo_info, pacman_files)


def _gather_files(roots: List[Path]) -> List[str]:
    files: List[str] = []
    while roots:
        if roots[0].is_dir():
            for current_path in roots[0].iterdir():
                if current_path.is_file():
                    files.append(str(current_path))
                if current_path.is_dir():
                    roots.append(current_path)
        roots = roots[1:]
    return files


def scan(root: Path,
         check_pacman: bool = False,
         ignore_paths: Optional[List[Path]] = None) -> Tuple[List[Path],
                                                             List[GitRepo], List[PacmanFile]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    _, paths, repos, pacman_files = asyncio.run(
        _git_crawl(
            root,
            _ScanConfig(
                check_pacman=check_pacman,
                ignore_paths=ignore_paths)))

    return (paths, repos, pacman_files)
