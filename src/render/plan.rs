use super::{counted, Cell, Grid, View};
use std::fmt::{Display, Formatter, Write};

/// Query Explain's text of a pipeline. Firestore documents neither the format nor its stability,
/// so the text is shown as it comes, coloured line by line, and a line this does not recognise is
/// printed plain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPlan {
    text: String,
}

/// What a run read, from the `Execution` and `Billing` sections that `EXPLAIN ANALYZE` adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExecutionStats {
    pub documents_read: Option<u64>,
    pub read_units: Option<u64>,
}

const TABLE_SCAN_NODE: &str = "TableScan";

impl QueryPlan {
    pub fn new(text: String) -> Self {
        Self { text }
    }

    pub fn stats(&self) -> ExecutionStats {
        let mut stats = ExecutionStats::default();
        let mut section = "";
        for line in self.text.lines() {
            if let Some(header) = line.strip_suffix(':').filter(|_| !line.starts_with(' ')) {
                section = header;
                continue;
            }
            let Some((key, value)) = line.trim().split_once(": ") else {
                continue;
            };
            let number = value.replace(',', "").parse::<u64>().ok();
            match (section, key) {
                ("Execution", "entity row scanned") => stats.documents_read = number,
                ("Billing", "read units") => stats.read_units = number,
                _ => {}
            }
        }
        stats
    }
}

impl Grid {
    pub fn plan(plan: &QueryPlan) -> Self {
        let mut grid = Grid::new(["text"]);
        grid.push_row(vec![Some(Cell::Text(plan.text.clone()))]);
        grid
    }
}

enum PlanLine<'a> {
    Node(PlanNode<'a>),
    Property(PlanProperty<'a>),
    Other(PlanText<'a>),
}

/// The indentation and tree-drawing characters that start a line. Firestore draws the verticals
/// with `|`, shown as `│` to join the `└──` corners.
struct TreePrefix<'a>(&'a str);

impl Display for TreePrefix<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0
            .chars()
            .try_for_each(|c| f.write_char(if c == '|' { '│' } else { c }))
    }
}

/// A `• Name rest` line, a node of the tree.
struct PlanNode<'a> {
    prefix: TreePrefix<'a>,
    name: &'a str,
    rest: &'a str,
}

/// A `key: value` line of a node.
struct PlanProperty<'a> {
    prefix: TreePrefix<'a>,
    key: &'a str,
    value: &'a str,
}

struct PlanText<'a> {
    prefix: TreePrefix<'a>,
    text: &'a str,
}

impl<'a> From<&'a str> for PlanLine<'a> {
    fn from(line: &'a str) -> Self {
        let text = line.trim_start_matches([' ', '|', '│', '└', '├', '─']);
        let prefix = TreePrefix(&line[..line.len() - text.len()]);
        if let Some(node) = text.strip_prefix("• ") {
            let (name, rest) = node.split_at(node.find(' ').unwrap_or(node.len()));
            return Self::Node(PlanNode { prefix, name, rest });
        }
        match text.split_once(": ") {
            // `$name: expression` binds a variable, drawn as plain text rather than as a property.
            Some((key, value)) if !key.contains(['(', '$']) => {
                Self::Property(PlanProperty { prefix, key, value })
            }
            _ => Self::Other(PlanText { prefix, text }),
        }
    }
}

impl View<'_> {
    pub fn plan(&self, plan: &QueryPlan) -> String {
        let theme = self.theme;
        let mut out = String::new();
        let mut in_tree = false;
        let mut in_node_execution = false;
        let mut in_table_scan = false;
        let mut table_scans = Vec::new();
        for line in plan.text.lines() {
            if !line.starts_with(' ') && line.ends_with(':') && !line.starts_with(['|', '└', '├'])
            {
                in_tree = line == "Tree:";
                out.push_str(&theme.header.apply_to(line).to_string());
                out.push('\n');
                continue;
            }
            if !in_tree {
                out.push_str(&self.stat_line(line));
                out.push('\n');
                continue;
            }
            let line = PlanLine::from(line);
            if let PlanLine::Property(PlanProperty {
                key: "source",
                value,
                ..
            }) = &line
            {
                if in_table_scan {
                    table_scans.push(*value);
                    in_table_scan = false;
                }
            }
            let rendered = match line {
                PlanLine::Node(PlanNode { prefix, name, rest }) => {
                    in_node_execution = false;
                    in_table_scan = name == TABLE_SCAN_NODE;
                    let style = if in_table_scan {
                        &theme.warning
                    } else {
                        &theme.plan_node
                    };
                    format!("{prefix}{}{rest}", style.apply_to(format!("• {name}")))
                }
                PlanLine::Other(PlanText { prefix, text }) if text == "Execution:" => {
                    in_node_execution = true;
                    format!("{prefix}{}", theme.muted.apply_to(text))
                }
                PlanLine::Other(PlanText { prefix, text: "" }) => {
                    in_node_execution = false;
                    prefix.to_string().trim_end().to_string()
                }
                PlanLine::Property(PlanProperty { prefix, key, value }) if in_node_execution => {
                    format!(
                        "{prefix}{}",
                        theme.muted.apply_to(format!("{key}: {value}"))
                    )
                }
                PlanLine::Property(PlanProperty {
                    prefix,
                    key: "index",
                    value,
                }) => format!(
                    "{prefix}{} {}",
                    theme.muted.apply_to("index:"),
                    theme.success.apply_to(value)
                ),
                PlanLine::Property(PlanProperty { prefix, key, value }) => {
                    format!(
                        "{prefix}{} {value}",
                        theme.muted.apply_to(format!("{key}:"))
                    )
                }
                PlanLine::Other(PlanText { prefix, text }) => format!("{prefix}{text}"),
            };
            out.push_str(&rendered);
            out.push('\n');
        }
        for source in table_scans {
            out.push_str(&format!(
                "{} a table scan reads every document of {source}; an index on the fields the statement filters or sorts on avoids it\n",
                theme.warning.apply_to("Warning:")
            ));
        }
        out
    }

    fn stat_line(&self, line: &str) -> String {
        match line.split_once(": ") {
            Some((key, value)) => {
                format!("{} {value}", self.theme.muted.apply_to(format!("{key}:")))
            }
            None => line.to_string(),
        }
    }
}

impl Display for ExecutionStats {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(documents) = self.documents_read {
            parts.push(counted(documents, "document read", "documents read"));
        }
        if let Some(units) = self.read_units {
            parts.push(counted(units, "read unit", "read units"));
        }
        if parts.is_empty() {
            f.write_str("no execution stats")
        } else {
            f.write_str(&parts.join(" · "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ExpandedMode, OutputFormat, Theme};

    /// `EXPLAIN SELECT title FROM books WHERE pages > 10`, which no index serves.
    const TABLE_SCAN_PLAN: &str = r#"Tree:
• Compute
|  $out_1: map("title", $title_1)
|  is query result: true
|
└── • Compute
    |  $title_1: $title_2
    |
    └── • TableScan
           source: /books
           order: UNDEFINED
           filter: (row().pages > 10L)
           output bindings: {$title_2=row().title}
           output: [$title_2]
"#;

    /// `EXPLAIN ANALYZE SELECT title FROM books WHERE genre = "Dystopian"`, which an index on
    /// genre serves.
    const INDEXED_ANALYZE_PLAN: &str = r#"Execution:
 results returned: 2
 query id: d4205b232de07783
 data bytes read: 879 B
 entity row scanned: 1,502
 index row scanned: 2

Billing:
 read units: 3

Tree:
• Compute
|  $out_1: map("title", $title_1)
|  is query result: true
|
|  Execution:
|   records returned: 2
|   latency: 8.63 ms (local 1.31 ms)
|
└── • Fetch
    |  order: UNDEFINED
    |  source: /books
    |  input record id: $rid_1
    |
    |  Execution:
    |   records returned: 2
    |   data bytes read: 669 B
    |
    └── • SequentialScan
           index: /books (genre ASC, rating DESC)@[id = CICAgPifq5IJ]
           ranges: /
                   |----["Dystopian"]

           Execution:
            records returned: 2
            records scanned: 2
"#;

    fn rendered(text: &str) -> String {
        View {
            theme: &Theme::new(false),
            format: OutputFormat::Table,
            expanded: ExpandedMode::Off,
            width: None,
            documents_path: "",
        }
        .plan(&QueryPlan::new(text.to_string()))
    }

    #[test]
    fn a_table_scan_plan_renders_as_a_tree_with_a_warning_naming_its_source() {
        assert_eq!(rendered(TABLE_SCAN_PLAN), RENDERED_TABLE_SCAN_PLAN);
    }

    #[test]
    fn an_indexed_plan_has_no_table_scan_to_warn_about() {
        assert!(!rendered(INDEXED_ANALYZE_PLAN).contains("Warning:"));
    }

    #[test]
    fn analyze_stats_come_from_the_execution_and_billing_sections() {
        assert_eq!(
            QueryPlan::new(INDEXED_ANALYZE_PLAN.to_string()).stats(),
            ExecutionStats {
                documents_read: Some(1502),
                read_units: Some(3),
            }
        );
        assert_eq!(
            QueryPlan::new(TABLE_SCAN_PLAN.to_string()).stats(),
            ExecutionStats::default()
        );
    }

    const RENDERED_TABLE_SCAN_PLAN: &str = r#"Tree:
• Compute
│  $out_1: map("title", $title_1)
│  is query result: true
│
└── • Compute
    │  $title_1: $title_2
    │
    └── • TableScan
           source: /books
           order: UNDEFINED
           filter: (row().pages > 10L)
           output bindings: {$title_2=row().title}
           output: [$title_2]
Warning: a table scan reads every document of /books; an index on the fields the statement filters or sorts on avoids it
"#;
}
