//! Minimal RFC 4180 CSV reader (quoted fields, `""` escapes, commas and newlines inside quotes,
//! CRLF), enough for the OurAirports files. No dependency.

/// Parse CSV text into rows of fields.
pub fn parse(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut line = 1usize;
    let mut any = false;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                any = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                any = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                if any || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                any = false;
            }
            c => {
                field.push(c);
                any = true;
            }
        }
    }
    if quoted {
        return Err(format!("unterminated quoted field (line {line})"));
    }
    if any || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// A CSV table with a header row: field access by column name.
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn parse(text: &str, label: &str) -> Result<Self, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut rows = parse(text).map_err(|e| format!("{label}: {e}"))?;
        if rows.is_empty() {
            return Err(format!("{label}: empty file"));
        }
        let header = rows.remove(0);
        Ok(Self { header, rows })
    }

    /// Column index of `name`, or an error naming the file's columns.
    pub fn col(&self, name: &str, label: &str) -> Result<usize, String> {
        self.header
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("{label}: no column `{name}` (columns: {})", self.header.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_commas_newlines() {
        let t = "a,b,c\r\n1,\"x, y\",\"say \"\"hi\"\"\"\n2,,\"multi\nline\"\n\n3,z,\n";
        let r = parse(t).unwrap();
        assert_eq!(r.len(), 4);
        assert_eq!(r[1], ["1", "x, y", "say \"hi\""]);
        assert_eq!(r[2], ["2", "", "multi\nline"]);
        assert_eq!(r[3], ["3", "z", ""]);
        assert!(parse("a,\"b").is_err());
        let tb = Table::parse("\u{feff}id,name\n1,x\n", "t").unwrap();
        assert_eq!(tb.col("name", "t").unwrap(), 1);
        assert!(tb.col("nope", "t").unwrap_err().contains("columns: id, name"));
    }
}
