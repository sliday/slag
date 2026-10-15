# Parse errors point at the wrong line when the file has comments

We validate `.hawk` configs in CI with this tool. The reported error
line is wrong whenever the file contains comment lines.

Example config:

    # server settings
    host = example.com
    port = 8080
    = oops

Running the CLI on it prints:

    error at line 3: unexpected '='

Line 3 is `port = 8080`, which is fine. The real problem is line 4.
With a longer comment header the offset grows: three comment lines and
the reported line is off by three. Files without any comments report
the right line.

Expected: the error line always matches the line in the file where the
problem actually is.

Actual: comment lines seem to be invisible to the line counter, so
every line after a comment is reported one number too low.
