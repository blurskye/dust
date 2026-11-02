def hello(name):
    """A friendly greeting for name."""
    return f"Hello, {name}!"


def shout(name):
    """The same greeting, louder."""
    return hello(name).upper()
