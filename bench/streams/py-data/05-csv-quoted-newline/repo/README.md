# csvsplit

Split CSV text into rows and fields.

`parse(csv_text)` returns a list of rows; each row is a list of field
strings and the first row is the header. Fields may be quoted with
double quotes, and quoted fields may contain commas and newlines.

## Tests

    python3 -m unittest discover
