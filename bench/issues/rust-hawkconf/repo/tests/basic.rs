use hawkconf::{parse, Token};

#[test]
fn parses_simple_entries() {
    let entries = parse("host = example.com\nport = 8080\n").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].key, "host");
    assert_eq!(entries[0].value, "example.com");
    assert_eq!(entries[0].line, 1);
    assert_eq!(entries[1].key, "port");
    assert_eq!(entries[1].line, 2);
}

#[test]
fn lexes_words_and_equals() {
    let tokens = hawkconf::lex("host = example.com\n");
    assert_eq!(
        tokens,
        vec![
            Token::Word {
                line: 1,
                text: "host".to_string()
            },
            Token::Equals { line: 1 },
            Token::Word {
                line: 1,
                text: "example.com".to_string()
            },
        ]
    );
}

#[test]
fn error_line_is_correct_without_comments() {
    let err = parse("host = example.com\n= oops\n").unwrap_err();
    assert_eq!(err.line, 2);
    assert_eq!(err.message, "unexpected '='");
}

#[test]
fn missing_value_is_an_error() {
    let err = parse("host =\n").unwrap_err();
    assert_eq!(err.line, 1);
    assert_eq!(err.message, "expected a value, found end of input");
}
