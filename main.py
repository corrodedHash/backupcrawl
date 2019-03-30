"""Main file"""
from typing import Iterator
import os


def breadth_first_file_scan(root: str) -> Iterator[str]:
    """ Iterates directory tree breadth first"""
    dirs = [root]
    # while we has dirs to scan
    while dirs:
        next_dirs = []
        for parent in dirs:
            # scan each dir
            for current_file in os.listdir(parent):
                # if there is a dir, then save for next ittr
                # if it  is a file then yield it (we'll return later)
                current_file_path = os.path.join(parent, current_file)
                if os.path.isdir(current_file_path):
                    next_dirs.append(current_file_path)
                else:
                    yield current_file_path
        # once we've done all the current dirs then
        # we set up the next itter as the child dirs
        # from the current itter.
        dirs = next_dirs

# -------------------------------------------
# an example func that just outputs the files.


def walkbf(path: str) -> None:
    """Main function"""
    for current_file in breadth_first_file_scan(path):
        print(current_file)


walkbf('/home/lukas/')
