"""Contains crawling functions"""

import logging
import os
from fnmatch import fnmatch
from pathlib import Path
from typing import List, Optional
from .crawlresult import CrawlResult

from .git_check import git_check_root
from .pacman_check import is_pacman_file
from .sync_status import BackupEntry, SyncStatus
from .statustracker import StatusTracker, VoidStatusTracker, TimingStatusTracker

MODULE_LOGGER = logging.getLogger("backupcrawl.crawler")


def _check_file(path: Path) -> BackupEntry:
    return is_pacman_file(path)


def _check_directory(path: Path) -> BackupEntry:
    return git_check_root(path)


def _dir_crawl(
    root: Path, ignore_paths: List[str], status: StatusTracker
) -> CrawlResult:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result = CrawlResult()
    result.path = root
    found_files = []
    recurse_dirs = []

    for current_path in root.iterdir():
        if any(fnmatch(str(current_path), cur_pattern) for cur_pattern in ignore_paths):
            continue

        if current_path.is_symlink():
            continue

        if current_path.is_file():
            if not os.access(current_path, os.R_OK):
                result.denied_paths.append(current_path)
                continue
            found_files.append(current_path)
            continue

        if not current_path.is_dir():
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
            continue

        if not os.access(current_path, os.R_OK | os.X_OK):
            result.denied_paths.append(current_path)
            continue

        backup_result = _check_directory(current_path)
        if backup_result.status != SyncStatus.NONE:
            result.add_backup(backup_result)
            continue

        recurse_dirs.append(current_path)

    status.open_paths(recurse_dirs)
    status.open_paths(found_files)

    for vcs_file in found_files:
        backup_result = _check_file(vcs_file)
        if backup_result.status == SyncStatus.NONE:
            result.loose_paths.append(backup_result.path)
        else:
            result.add_backup(backup_result)
        status.close_path(vcs_file)

    for recurse_dir in recurse_dirs:
        result.extend(_dir_crawl(recurse_dir, ignore_paths, status))
        status.close_path(recurse_dir)

    return result


def scan(
    root: Path, ignore_paths: Optional[List[str]] = None, progress: bool = False
) -> CrawlResult:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    if progress:
        status = TimingStatusTracker(root)
    else:
        status = VoidStatusTracker(root)

    crawl_result = _dir_crawl(root, ignore_paths, status)

    return crawl_result
