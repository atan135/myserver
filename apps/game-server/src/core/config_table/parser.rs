pub fn parse_csv_columns(line: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut current = String::new();
    let mut generic_depth = 0u32;
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if chars.peek() == Some(&'"') {
                        current.push('"');
                        chars.next();
                    } else {
                        in_quotes = false;
                    }
                } else {
                    in_quotes = true;
                }
            }
            '<' => {
                generic_depth += 1;
                current.push(ch);
            }
            '>' => {
                generic_depth = generic_depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if !in_quotes && generic_depth == 0 => {
                columns.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    columns.push(current.trim().to_string());
    if let Some(first) = columns.first_mut() {
        *first = first.trim_start_matches('﻿').to_string();
    }

    columns
}

pub fn schema_signature(headers: &[String], types: &[String]) -> String {
    headers
        .iter()
        .zip(types.iter())
        .map(|(header, ty)| format!("{header}:{ty}"))
        .collect::<Vec<_>>()
        .join("|")
}
