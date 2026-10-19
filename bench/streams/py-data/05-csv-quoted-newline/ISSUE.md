# Address export with commas and line breaks falls apart

I export our customer list as CSV and parse it with `parse()`. Rows
where a field is quoted and contains a comma or a line break come back
shredded into extra rows and extra fields.

Here is the exact file content, byte for byte (it ends with a newline
after the last line):

```
name,address,note
"Smith, John","12 Main St
Apt 4",vip
"Doe, Jane",5 Oak Ave,regular
Brown,9 Pine Rd,new
"Adams, Amy","77 Lake View
Suite 2",vip
```

That is a header plus four data records. The first and last records
have quoted fields: `"Smith, John"` contains a comma, and the address
`"12 Main St` / `Apt 4"` spans two lines inside the quotes.

Expected: `parse()` returns 5 rows. Row 1 is
`["Smith, John", "12 Main St\nApt 4", "vip"]` and row 4 is
`["Adams, Amy", "77 Lake View\nSuite 2", "vip"]`.

Actual: I get 7 rows. The comma inside `"Smith, John"` splits the name
into two fields, and each embedded line break starts a bogus new row
like `["Apt 4\",vip"]`.

Plain rows with no quotes, like the `Brown` record, parse fine.
