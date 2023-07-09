"""Contains crawling functions"""

import logging
import os
from fnmatch import fnmatch
from pathlib import Path

from .crawlresult import CrawlResult
from .git_check import GitDirChecker
from .pacman_check import PacmanFileChecker
from .statustracker import StatusTracker, VoidStatusTracker
from .sync_status import BackupEntry, DirChecker, FileChecker, SyncStatus


MODULE_LOGGER = logging.getLogger("backupcrawl.crawler")


def _check_file(path: Path, file_checks: list[FileChecker]) -> BackupEntry:
    for check in file_checks:
        status = check.check_file(path)
        if status.status != SyncStatus.NONE:
            return status
    return BackupEntry(path, SyncStatus.NONE)


def _check_directory(path: Path, dir_checks: list[DirChecker]) -> BackupEntry:
    for check in dir_checks:
        status = check.check_dir(path)
        if status.status != SyncStatus.NONE:
            return status
    return BackupEntry(path, SyncStatus.NONE)


def _filter_directory(
    root: Path,
    ignore_paths: list[str],
    result: CrawlResult,
) -> tuple[list[Path], list[Path]]:
    found_files: list[Path] = []
    recurse_dirs: list[Path] = []

    def is_ignored(check_path: Path) -> bool:
        return any(
            fnmatch(str(check_path), os.path.expanduser(cur_pattern))
            for cur_pattern in ignore_paths
        )

    filtered_paths = (x for x in root.iterdir() if not is_ignored(x))
    non_symlinks = (x for x in filtered_paths if not x.is_symlink())
    for current_path in non_symlinks:
        if current_path.is_file():
            if not os.access(current_path, os.R_OK):
                result.denied_paths.append(current_path)
                continue

            found_files.append(current_path)

        elif current_path.is_dir():
            if not os.access(current_path, os.R_OK | os.X_OK):
                result.denied_paths.append(current_path)
                continue

            recurse_dirs.append(current_path)
        else:
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
            pass
    return (found_files, recurse_dirs)


def _dir_crawl(
    root: Path,
    ignore_paths: list[str],
    status: StatusTracker,
    checks: tuple[list[DirChecker], list[FileChecker]],
) -> CrawlResult:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result = CrawlResult(root)
    status.current_path(root)

    backup_result = _check_directory(root, checks[0])
    if backup_result.status != SyncStatus.NONE:
        result.add_backup(backup_result)
        return result

    (found_files, recurse_dirs) = _filter_directory(root, ignore_paths, result)

    status.open_paths(recurse_dirs)
    status.open_paths(found_files)

    for vcs_file in found_files:
        backup_result = _check_file(vcs_file, checks[1])
        if backup_result.status == SyncStatus.NONE:
            result.loose_paths.append(backup_result.path)
        else:
            result.add_backup(backup_result)
        status.close_path(vcs_file)

    for recurse_dir in recurse_dirs:
        result.extend(_dir_crawl(recurse_dir, ignore_paths, status, checks))
        status.close_path(recurse_dir)

    return result


def scan(
    root: Path,
    ignore_paths: list[str] | None = None,
    status: StatusTracker | None = None,
) -> CrawlResult:
    """Scan the given path for files that are not backed up"""
    if ignore_paths is None:
        ignore_paths = []
    if status is None:
        status = VoidStatusTracker(root)
    with status as entered_status:
        crawl_result = _dir_crawl(
            root,
            ignore_paths,
            entered_status,
            ([GitDirChecker()], [PacmanFileChecker()]),
        )

    return crawl_result
