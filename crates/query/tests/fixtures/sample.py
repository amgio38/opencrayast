"""Module doc."""

MAX = 10


def add(a, b):
    """Adds."""
    return a + b


class Config:
    """A config."""

    def load(self, path):
        return path

    @staticmethod
    def make():
        return Config()


@decorator
def decorated():
    pass
