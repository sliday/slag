# ledgercsv

Read CSV ledger exports (columns: date, description, amount) into row
dicts and compute simple summaries.

## Usage

    from ledgercsv import read_rows, total_amount

    rows = read_rows("export.csv")
    print(rows[0]["date"], total_amount(rows))

## Tests

    python3 -m unittest discover
