"""Split CSV text into rows and fields."""


def parse(csv_text):
    """Parse CSV text into a list of rows.

    Each row is a list of field strings. The first row is the header.
    """
    rows = []
    for line in csv_text.split("\n"):
        if line:
            rows.append(line.split(","))
    return rows
