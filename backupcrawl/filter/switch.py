from pathlib import Path
from typing import Callable, Union, Iterator, List, Tuple, Optional
from .base import FilterType

FilterChain = FilterType#Tuple[List[Union[FilterType], Optional[Switch]]]

class Switch:
    def __init__(self, decision: Callable[[Path], bool]):
        self.true_branch: FilterChain = ([], None)
        self.false_branch: FilterChain = ([], None)
        self.decision: Callable[[Path], bool] = decision

    def get_branch(self, filepath: Path) -> FilterChain:
        if self.decision(filepath):
            return self.true_branch
        return self.false_branch
