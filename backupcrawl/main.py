"""Main file"""
import logging
from pathlib import Path
from . import backupcrawler
from .backupcrawler import GitSyncStatus, PacmanSyncStatus


def main(path: str) -> None:
    """Main function"""
    ignore_paths = list(map(Path, ["/home/lukas/.npm",
                                   "/home/lukas/.cache",
                                   "/home/lukas/.mypy_cache",
                                   "/home/lukas/.aurget_build",
                                   "/home/lukas/.debug",
                                   "/home/lukas/.vscode",
                                   "/home/lukas/.vim/bundle", ]))
    sync_tree, git_repos, pacman_files = backupcrawler.scan(
        Path(path), ignore_paths=ignore_paths)
    for standard_file in sync_tree:
        print("\t" + str(standard_file))

    for enum_state, status_string in (
            (GitSyncStatus.DIRTY, "Dirty repositories"),
            (GitSyncStatus.AHEAD, "Unsynced repositories"),
            (GitSyncStatus.CLEAN_SYNCED, "Clean repositories")):
        print(status_string + ":")
        for git_dir in [t.path for t in git_repos
                        if t.status == enum_state]:
            print("\t" + str(git_dir))

    print("Managed by pacman:")
    for pacman_status, status_string in (
            (PacmanSyncStatus.CHANGED, "Changed pacman files"),
            (PacmanSyncStatus.CLEAN, "Clean pacman files")):
        print(status_string + ":")
        for pacman_file in [t for t in pacman_files
                            if t.status == pacman_status]:
            print("\t" + str(pacman_file.path) + " " + pacman_file.package)


if __name__ == "__main__":
    logging.basicConfig(level="DEBUG")
    # walkbf('/home/lukas')
    main('/etc')
    # walkbf('/home/lukas/Downloads')
