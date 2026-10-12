//! Saving the report as XLSX or CSV.
//!
//! A text cell that starts with =, +, -, @, tab or carriage return is
//! written as text, so a saved file can't run anything in Excel: in XLSX
//! every text cell is a string cell (never a formula); in CSV such a cell is
//! prefixed with an apostrophe, which spreadsheet programs show as text.

use std::path::Path;

use rust_xlsxwriter::{Format, Workbook};

use crate::report::{Report, ReportRow};

const HEADERS: [&str; 12] = [
    "Item",
    "Variation",
    "SKU",
    "Category",
    "Last sold",
    "Days since",
    "Slow",
    "On hand",
    "Value at price",
    "Value at cost",
    "Currency",
    "Label in Square",
];

/// Text that a spreadsheet might read as a formula.
pub fn risky(text: &str) -> bool {
    text.starts_with(['=', '+', '-', '@', '\t', '\r'])
}

fn money(cents: Option<i64>) -> Option<f64> {
    cents.map(|c| c as f64 / 100.0)
}

fn texts(row: &ReportRow) -> [String; 5] {
    [
        row.item.clone(),
        row.variation.clone(),
        row.sku.clone().unwrap_or_default(),
        row.category.clone().unwrap_or_default(),
        row.last_sold.clone(),
    ]
}

pub fn write_csv(report: &Report, path: &Path) -> std::io::Result<()> {
    let mut out = String::from("\u{feff}");
    out.push_str(&HEADERS.join(","));
    out.push_str("\r\n");
    for row in &report.rows {
        let [item, variation, sku, category, last] = texts(row);
        let cells: Vec<String> = vec![
            csv_text(&item),
            csv_text(&variation),
            csv_text(&sku),
            csv_text(&category),
            csv_text(&last),
            row.days_since.map(|d| d.to_string()).unwrap_or_default(),
            if row.slow { "yes".into() } else { "no".into() },
            row.on_hand.map(format_quantity).unwrap_or_default(),
            money(row.value_at_price).map(|m| format!("{m:.2}")).unwrap_or_default(),
            money(row.value_at_cost).map(|m| format!("{m:.2}")).unwrap_or_default(),
            csv_text(row.currency.as_deref().unwrap_or("")),
            csv_text(&row.label_status),
        ];
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    std::fs::write(path, out)
}

fn format_quantity(q: f64) -> String {
    if q.fract() == 0.0 { format!("{q:.0}") } else { format!("{q}") }
}

fn csv_text(text: &str) -> String {
    let safe = if risky(text) { format!("'{text}") } else { text.to_string() };
    if safe.contains([',', '"', '\n', '\r']) { format!("\"{}\"", safe.replace('"', "\"\"")) } else { safe }
}

pub fn write_xlsx(report: &Report, path: &Path) -> Result<(), rust_xlsxwriter::XlsxError> {
    let mut book = Workbook::new();
    let sheet = book.add_worksheet();
    sheet.set_name("Dead stock")?;
    let bold = Format::new().set_bold();
    let money_format = Format::new().set_num_format("#,##0.00");
    for (c, h) in HEADERS.iter().enumerate() {
        sheet.write_string_with_format(0, c as u16, *h, &bold)?;
    }
    for (i, row) in report.rows.iter().enumerate() {
        let r = (i + 1) as u32;
        let [item, variation, sku, category, last] = texts(row);
        // Strings are always string cells: nothing becomes a formula.
        for (c, text) in [item, variation, sku, category, last].iter().enumerate() {
            sheet.write_string(r, c as u16, text)?;
        }
        if let Some(d) = row.days_since {
            sheet.write_number(r, 5, d as f64)?;
        }
        sheet.write_string(r, 6, if row.slow { "yes" } else { "no" })?;
        if let Some(q) = row.on_hand {
            sheet.write_number(r, 7, q)?;
        }
        if let Some(m) = money(row.value_at_price) {
            sheet.write_number_with_format(r, 8, m, &money_format)?;
        }
        if let Some(m) = money(row.value_at_cost) {
            sheet.write_number_with_format(r, 9, m, &money_format)?;
        }
        sheet.write_string(r, 10, row.currency.as_deref().unwrap_or(""))?;
        sheet.write_string(r, 11, &row.label_status)?;
    }
    for (c, width) in [32, 20, 14, 18, 30, 10, 6, 9, 14, 14, 8, 28].iter().enumerate() {
        sheet.set_column_width(c as u16, *width)?;
    }
    sheet.set_freeze_panes(1, 0)?;
    if !report.rows.is_empty() {
        sheet.autofilter(0, 0, report.rows.len() as u32, (HEADERS.len() - 1) as u16)?;
    }
    book.save(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(item: &str) -> ReportRow {
        ReportRow {
            item_id: "I1".into(),
            variation_id: "V1".into(),
            item: item.into(),
            variation: "Blue".into(),
            sku: Some("+SKU".into()),
            category: None,
            last_sold: "Jul 14, 2026".into(),
            last_sold_date: Some("2026-07-14".into()),
            days_since: Some(90),
            slow: false,
            on_hand: Some(4.0),
            currency: Some("USD".into()),
            value_at_price: Some(4800),
            value_at_cost: None,
            label_status: "In Square".into(),
        }
    }

    fn report(rows: Vec<ReportRow>) -> Report {
        Report { rows, window_days: 90, categories: vec![], slow_count: 0, items_shown: 1 }
    }

    #[test]
    fn csv_neutralises_formulas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.csv");
        write_csv(
            &report(vec![
                row("=HYPERLINK(\"http://x\",\"y\")"),
                row("@SUM(A1)"),
                row("\tTab"),
                row("Mug, large"),
            ]),
            &path,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(text.contains("\"'=HYPERLINK(\"\"http://x\"\",\"\"y\"\")\""));
        assert!(text.contains("'@SUM(A1)"));
        assert!(text.contains("'\tTab"));
        assert!(text.contains("'+SKU"));
        assert!(text.contains("\"Mug, large\""));
        assert!(text.contains(",48.00,"));
        for line in text.lines().skip(1) {
            for cell in line.split(',') {
                let c = cell.trim_start_matches('"');
                assert!(!(c.starts_with('=') || c.starts_with('@') || c.starts_with('+')), "{line}");
            }
        }
    }

    #[test]
    fn xlsx_has_no_formula_cells() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.xlsx");
        write_xlsx(&report(vec![row("=1+1"), row("-2"), row("Plain")]), &path).unwrap();
        // Unzip the sheet and confirm there is no <f> (formula) element.
        let file = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut sheet = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("xl/worksheets/sheet1.xml").unwrap(), &mut sheet)
            .unwrap();
        assert!(!sheet.contains("<f>"), "formula cell written");
        assert!(sheet.contains("t=\"s\""));
    }
}
