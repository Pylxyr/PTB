//! A plain-text report that is easy to paste back into a chat or an issue: titled sections of lines, and
//! one fixed layout for latency rows so two reports line up when compared.

use crate::stats::Summary;

#[derive(Debug, Default)]
pub struct Section {
    heading: String,
    lines: Vec<String>,
}

impl Section {
    pub fn line(&mut self, text: impl Into<String>) -> &mut Self {
        self.lines.push(text.into());
        self
    }

    /// `label: value`, the label padded so values line up inside a section.
    pub fn kv(&mut self, label: &str, value: impl std::fmt::Display) -> &mut Self {
        self.lines.push(format!("{label:<22}{value}"));
        self
    }

    /// One latency row: `label  n=20  min  p50  p95  p99  max  (ms)`.
    pub fn latency(&mut self, label: &str, summary: &Summary) -> &mut Self {
        self.lines.push(latency_row(label, summary));
        self
    }
}

#[derive(Debug)]
pub struct Report {
    title: String,
    sections: Vec<Section>,
}

impl Report {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            sections: Vec::new(),
        }
    }

    pub fn section(&mut self, heading: impl Into<String>) -> &mut Section {
        self.sections.push(Section {
            heading: heading.into(),
            lines: Vec::new(),
        });
        self.sections.last_mut().expect("a section was just pushed")
    }

    pub fn render(&self) -> String {
        let mut out = format!("{}\n", self.title);
        for section in &self.sections {
            out.push_str(&format!("\n== {} ==\n", section.heading));
            for line in &section.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}

/// The fixed layout of a latency row, in milliseconds with one decimal.
pub fn latency_row(label: &str, s: &Summary) -> String {
    format!(
        "{label:<22}n={:<3} min {:>7.1}  p50 {:>7.1}  p95 {:>7.1}  p99 {:>7.1}  max {:>7.1}  ms",
        s.count, s.min, s.p50, s.p95, s.p99, s.max
    )
}

/// KiB as MiB with one decimal, for memory lines.
pub fn mib(kib: u64) -> String {
    format!("{:.1} MiB", kib as f64 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::summarize;

    #[test]
    fn report_renders_title_then_sections_in_order() {
        let mut r = Report::new("PyxeeTB probe");
        r.section("Host").kv("cpu", "AMD").kv("memory", "1 GiB");
        r.section("Notes").line("free text");
        assert_eq!(
            r.render(),
            "PyxeeTB probe\n\n== Host ==\ncpu                   AMD\nmemory                1 GiB\n\n== Notes ==\nfree text\n"
        );
    }

    #[test]
    fn latency_rows_have_a_fixed_layout() {
        let s = summarize(&[10.0, 20.0, 30.0, 40.0]).unwrap();
        let row = latency_row("tcp connect", &s);
        assert!(row.starts_with("tcp connect           n=4  "), "{row}");
        assert!(
            row.contains("min    10.0") && row.contains("max    40.0"),
            "{row}"
        );
        assert!(row.ends_with("ms"));
    }

    #[test]
    fn long_labels_do_not_panic_or_truncate() {
        let s = summarize(&[1.0]).unwrap();
        assert!(latency_row("a-very-long-label-indeed", &s).starts_with("a-very-long-label-indeed"));
    }

    #[test]
    fn mib_formats_one_decimal() {
        assert_eq!(mib(1024), "1.0 MiB");
        assert_eq!(mib(958_292), "935.8 MiB");
    }
}
