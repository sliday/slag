# nameset

Merge name lists from several import sources.

`unique_names(names)` removes duplicates and keeps the first occurrence
of each name, in the order names first appear. Two strings that spell
the same name count as duplicates even when they differ in Unicode
normalization form.

## Tests

    python3 -m unittest discover
