use hawkconf::parse;

#[test]
fn error_line_accounts_for_comment_lines() {
    let src = "# server settings\nhost = example.com\nport = 8080\n= oops\n";
    let err = parse(src).unwrap_err();
    assert_eq!(err.line, 4);
}

#[test]
fn each_comment_shifts_the_reported_line() {
    let src = "# a\n# b\n# c\nhost = example.com\n= bad\n";
    let err = parse(src).unwrap_err();
    assert_eq!(err.line, 5);
}

#[test]
fn entries_after_comments_carry_their_true_line() {
    let src = "# top\nhost = example.com\n# middle\nport = 8080\n";
    let entries = parse(src).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].line, 2);
    assert_eq!(entries[1].line, 4);
}

#[test]
fn comment_only_file_parses_empty() {
    let entries = parse("# nothing here\n# really\n").unwrap();
    assert_eq!(entries.len(), 0);
}

#[test]
fn trailing_comment_without_newline_is_fine() {
    let entries = parse("host = example.com\n# done").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].value, "example.com");
}
