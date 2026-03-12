pub mod doc;
pub mod printer;

use doc::render;

pub fn format(source: &str) -> String {
    format_width(source, 80)
}

pub fn format_width(source: &str, width: u32) -> String {
    let result = expo_parser::parse(source);
    let doc = printer::module_to_doc(&result.module);
    let rendered = render(&doc, width);
    let mut out: String = rendered
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
