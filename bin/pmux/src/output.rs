use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

pub fn json<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

pub fn ndjson<T: Serialize>(kind: &str, value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &Record { kind, data: value })?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

pub fn text(value: &str) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(value.as_bytes())?;
    if !value.ends_with('\n') {
        output.write_all(b"\n")?;
    }
    output.flush()?;
    Ok(())
}

#[derive(Serialize)]
struct Record<'a, T> {
    #[serde(rename = "type")]
    kind: &'a str,
    data: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndjson_record_has_stable_shape() {
        let encoded = serde_json::to_string(&Record {
            kind: "result",
            data: serde_json::json!({"text": "done"}),
        })
        .unwrap();
        assert_eq!(encoded, r#"{"type":"result","data":{"text":"done"}}"#);
    }
}
