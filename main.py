"""Main file"""
import logging
import backupcrawler
from backupcrawler import GitSyncStatus, PacmanSyncStatus


def walkbf(path: str) -> None:
    """Main function"""
    ignore_paths = ["/home/lukas/.npm",
                    "/home/lukas/.cache",
                    "/home/lukas/.mypy_cache",
                    "/home/lukas/.aurget_build",
                    "/home/lukas/.debug",
                    "/home/lukas/.vscode",
                    "/home/lukas/.vim/bundle", ]
    sync_tree = backupcrawler.scan(
        path, ignore_paths=ignore_paths, check_pacman=False)
    for current_file in sync_tree[1]:
        print("\t" + current_file)

    for enum_state, status_string in (
            (GitSyncStatus.DIRTY, "Dirty repositories"),
            (GitSyncStatus.AHEAD, "Unsynced repositories"),
            (GitSyncStatus.CLEAN_SYNCED, "Clean repositories")):
        print(status_string + ":")
        for current_file in [t.path for t in sync_tree[2]
                             if t.git_status == enum_state]:
            print("\t" + current_file)

    for pacman_status, status_string in (
            (PacmanSyncStatus.CHANGED, "Changed pacman file"),
            (PacmanSyncStatus.CLEAN, "Clean pacman file")):
        print(status_string + ":")
        for current_file in [t.path for t in sync_tree[2]
                             if t.pacman_status == pacman_status]:
            print("\t" + current_file)


logging.basicConfig(level="DEBUG")
walkbf('/home/lukas')
