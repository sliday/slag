"""Incremental CSV row iteration over fixed-size file chunks."""


def iter_rows(path, chunk_size=4096):
    """Yield rows from a CSV file, reading at most `chunk_size`
    characters from the file at a time.

    Each row is a list of field strings; the first row is the header.
    Fields may be quoted with double quotes, and quoted fields may
    contain commas and newlines.
    """
    buffer = ""
    with open(path, newline="") as f:
        while True:
            chunk = f.read(chunk_size)
            if not chunk:
                break
            buffer += chunk
            while "\n" in buffer:
                line, buffer = buffer.split("\n", 1)
                if line:
                    yield line.split(",")
    if buffer:
        yield buffer.split(",")
