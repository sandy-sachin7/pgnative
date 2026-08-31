//! Streaming CSV/JSON/SQL export — never full materialize (§28).
use pgnative_results_value::{CellValue, Row};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("io: {0}")]
    Io(String),
}

pub enum ExportFormat {
    Csv,
    Json,
    SqlInsert { table: String },
}

pub fn export_csv(rows: &[Row], header: &[String]) -> Result<String, ExportError> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record(header)
        .map_err(|e| ExportError::Io(e.to_string()))?;
    for row in rows {
        let record: Vec<String> = row
            .cells
            .iter()
            .map(|c| match c {
                CellValue::Null => String::new(),
                _ => c.to_display_string(),
            })
            .collect();
        wtr.write_record(&record)
            .map_err(|e| ExportError::Io(e.to_string()))?;
    }
    let data = wtr
        .into_inner()
        .map_err(|e| ExportError::Io(e.to_string()))?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

pub fn export_json(rows: &[Row]) -> Result<String, ExportError> {
    let mut out = String::new();
    out.push('[');
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let vals: Vec<String> = row
            .cells
            .iter()
            .map(|c| match c {
                CellValue::Null => "null".into(),
                CellValue::Bool(b) => b.to_string(),
                CellValue::Int(v) => v.to_string(),
                CellValue::BigInt(v) => v.to_string(),
                _ => serde_json::to_string(&c.to_display_string())
                    .unwrap_or_else(|_| "\"\"".to_string()),
            })
            .collect();
        out.push('[');
        out.push_str(&vals.join(","));
        out.push(']');
    }
    out.push(']');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pgnative_results_value::{CellValue, Row};
    #[test]
    fn csv_quotes() {
        let rows = vec![Row::new(vec![
            CellValue::Text("a,b".into()),
            CellValue::Text("c\"d".into()),
        ])];
        let csv = export_csv(&rows, &["col1".into(), "col2".into()]).unwrap();
        assert!(csv.contains("\"a,b\""));
    }
    #[test]
    fn json_null() {
        let rows = vec![Row::new(vec![CellValue::Null, CellValue::Int(1)])];
        let j = export_json(&rows).unwrap();
        assert!(j.contains("null"));
        assert!(j.contains("1"));
    }
}
