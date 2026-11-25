//! PostgreSQL query plans as a tree: what `EXPLAIN (FORMAT JSON)` returns,
//! parsed into nodes, with each node's own share of the work and the plan
//! written out as `EXPLAIN` writes it in text.

use serde_json::{Map, Value};
use std::collections::HashSet;

/// One step of a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    /// Its place in the plan, counting from the root in reading order.
    pub id: usize,
    /// The server's name for it, such as `Seq Scan` or `Hash Join`.
    pub node_type: String,
    /// What it is called in a text plan: `Hash Left Join`, `HashAggregate`…
    pub operation: String,
    /// What it reads: `on users u`, `using users_pkey on users u`.
    pub target: Option<String>,
    /// How it feeds its parent when that is not plain input: `InitPlan 1`, `SubPlan 2`.
    pub subplan: Option<String>,
    pub startup_cost: f64,
    pub total_cost: f64,
    pub plan_rows: f64,
    pub plan_width: u64,
    /// What really happened, with Analyze.
    pub actual: Option<Actual>,
    /// Conditions, keys and counters, as label and value: `Filter`, `Index Cond`…
    pub details: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
}

/// A node's measured run. Times are per loop, as the server reports them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Actual {
    pub startup_ms: f64,
    pub total_ms: f64,
    pub rows: f64,
    /// 0 when the node never ran.
    pub loops: f64,
}

/// A whole plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub root: PlanNode,
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
    /// Whether the statement ran (Analyze).
    pub analyzed: bool,
}

/// A node as a row of the tree.
#[derive(Debug, Clone, Copy)]
pub struct PlanRow<'a> {
    pub node: &'a PlanNode,
    pub depth: usize,
    /// The node's own part of the plan, 0 to 1: its time with Analyze, else
    /// its cost, without what its children account for.
    pub share: f64,
}

/// Conditions and keys shown with a node, in this order.
const DETAILS: &[&str] = &[
    "Index Cond",
    "Recheck Cond",
    "Hash Cond",
    "Merge Cond",
    "Join Filter",
    "Filter",
    "Rows Removed by Filter",
    "Rows Removed by Join Filter",
    "Rows Removed by Index Recheck",
    "Sort Key",
    "Group Key",
    "Presorted Key",
    "Sort Method",
    "One-Time Filter",
    "Heap Fetches",
    "Workers Planned",
    "Workers Launched",
];

impl Plan {
    /// Parses what `EXPLAIN (FORMAT JSON …)` returns.
    pub fn parse(json: &str) -> Result<Plan, String> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| format!("The plan is not JSON: {e}"))?;
        let top = match &value {
            Value::Array(items) => items.first(),
            Value::Object(_) => Some(&value),
            _ => None,
        }
        .and_then(Value::as_object)
        .ok_or("The plan is empty")?;
        let root = top
            .get("Plan")
            .and_then(Value::as_object)
            .ok_or("The plan has no nodes")?;
        let mut next = 0;
        let root = node(root, &mut next);
        let analyzed = root.actual.is_some();
        Ok(Plan {
            root,
            planning_ms: top.get("Planning Time").and_then(Value::as_f64),
            execution_ms: top.get("Execution Time").and_then(Value::as_f64),
            analyzed,
        })
    }

    /// The rows of the tree in reading order, leaving out what is under the
    /// nodes in `collapsed`.
    pub fn rows(&self, collapsed: &HashSet<usize>) -> Vec<PlanRow<'_>> {
        let whole = if self.analyzed {
            time(&self.root)
        } else {
            self.root.total_cost
        };
        let mut rows = Vec::new();
        walk(&self.root, 0, whole, self.analyzed, collapsed, &mut rows);
        rows
    }

    /// How many nodes it has.
    pub fn node_count(&self) -> usize {
        fn count(n: &PlanNode) -> usize {
            1 + n.children.iter().map(count).sum::<usize>()
        }
        count(&self.root)
    }

    /// The plan as `EXPLAIN` writes it in text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        text_of(&self.root, 0, &mut out);
        if let Some(ms) = self.planning_ms {
            out.push_str(&format!("Planning Time: {ms:.3} ms\n"));
        }
        if let Some(ms) = self.execution_ms {
            out.push_str(&format!("Execution Time: {ms:.3} ms\n"));
        }
        out.pop();
        out
    }
}

impl PlanNode {
    /// What a text plan says for its cost and size.
    pub fn estimate(&self) -> String {
        format!(
            "(cost={:.2}..{:.2} rows={} width={})",
            self.startup_cost,
            self.total_cost,
            count(self.plan_rows),
            self.plan_width
        )
    }

    /// The operation with what it reads, as one line.
    pub fn title(&self) -> String {
        let mut title = self.operation.clone();
        if let Some(target) = &self.target {
            title.push(' ');
            title.push_str(target);
        }
        title
    }
}

/// Rows as the server counts them: whole unless they are not.
pub fn count(rows: f64) -> String {
    if rows.fract() == 0. {
        format!("{rows:.0}")
    } else {
        format!("{rows:.2}")
    }
}

/// All the time a node took, over its loops.
fn time(node: &PlanNode) -> f64 {
    node.actual.map_or(0., |a| a.total_ms * a.loops)
}

fn walk<'a>(
    node: &'a PlanNode,
    depth: usize,
    whole: f64,
    analyzed: bool,
    collapsed: &HashSet<usize>,
    rows: &mut Vec<PlanRow<'a>>,
) {
    let (own, children) = if analyzed {
        (time(node), node.children.iter().map(time).sum::<f64>())
    } else {
        (
            node.total_cost,
            node.children.iter().map(|c| c.total_cost).sum::<f64>(),
        )
    };
    let share = if whole > 0. {
        ((own - children).max(0.) / whole).min(1.)
    } else {
        0.
    };
    rows.push(PlanRow { node, depth, share });
    if !collapsed.contains(&node.id) {
        for child in &node.children {
            walk(child, depth + 1, whole, analyzed, collapsed, rows);
        }
    }
}

fn text_of(node: &PlanNode, depth: usize, out: &mut String) {
    let pad = |n: usize| " ".repeat(n);
    let (head, body) = if depth == 0 {
        (String::new(), pad(2))
    } else {
        (format!("{}->  ", pad(6 * depth - 4)), pad(6 * depth + 2))
    };
    if let Some(subplan) = &node.subplan {
        out.push_str(&format!(
            "{}{subplan}\n",
            pad((6 * depth).saturating_sub(4))
        ));
    }
    out.push_str(&format!("{head}{}  {}", node.title(), node.estimate()));
    match node.actual {
        Some(a) if a.loops == 0. => out.push_str(" (never executed)"),
        Some(a) => out.push_str(&format!(
            " (actual time={:.3}..{:.3} rows={} loops={})",
            a.startup_ms,
            a.total_ms,
            count(a.rows),
            count(a.loops)
        )),
        None => {}
    }
    out.push('\n');
    for (label, value) in &node.details {
        out.push_str(&format!("{body}{label}: {value}\n"));
    }
    for child in &node.children {
        text_of(child, depth + 1, out);
    }
}

fn node(map: &Map<String, Value>, next: &mut usize) -> PlanNode {
    let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_owned);
    let number = |key: &str| map.get(key).and_then(Value::as_f64);
    let id = *next;
    *next += 1;
    let node_type = text("Node Type").unwrap_or_else(|| "Unknown".into());
    let actual = match (number("Actual Total Time"), number("Actual Loops")) {
        (Some(total_ms), Some(loops)) => Some(Actual {
            startup_ms: number("Actual Startup Time").unwrap_or(0.),
            total_ms,
            rows: number("Actual Rows").unwrap_or(0.),
            loops,
        }),
        // A node that never ran says only how often it looped.
        (None, Some(loops)) => Some(Actual {
            startup_ms: 0.,
            total_ms: 0.,
            rows: 0.,
            loops,
        }),
        _ => None,
    };
    let mut details: Vec<(String, String)> = DETAILS
        .iter()
        .filter_map(|key| {
            let value = map.get(*key)?;
            let value = match value {
                Value::String(s) => s.clone(),
                Value::Array(items) => items
                    .iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join(", "),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            // Counters of nothing are noise.
            (value != "0" || !key.starts_with("Rows Removed")).then(|| ((*key).to_owned(), value))
        })
        .collect();
    let blocks: Vec<String> = [
        ("Shared Hit Blocks", "hit"),
        ("Shared Read Blocks", "read"),
        ("Shared Dirtied Blocks", "dirtied"),
        ("Shared Written Blocks", "written"),
    ]
    .iter()
    .filter_map(|(key, name)| {
        let n = map.get(*key)?.as_u64()?;
        (n > 0).then(|| format!("{name}={n}"))
    })
    .collect();
    if !blocks.is_empty() {
        details.push(("Buffers".into(), format!("shared {}", blocks.join(" "))));
    }
    let children = map
        .get("Plans")
        .and_then(Value::as_array)
        .map(|plans| {
            plans
                .iter()
                .filter_map(Value::as_object)
                .map(|child| node(child, next))
                .collect()
        })
        .unwrap_or_default();
    PlanNode {
        id,
        operation: operation(map, &node_type),
        target: target(map, &node_type),
        subplan: text("Subplan Name"),
        node_type,
        startup_cost: number("Startup Cost").unwrap_or(0.),
        total_cost: number("Total Cost").unwrap_or(0.),
        plan_rows: number("Plan Rows").unwrap_or(0.),
        plan_width: map.get("Plan Width").and_then(Value::as_u64).unwrap_or(0),
        actual,
        details,
        children,
    }
}

/// The node's name as a text plan gives it.
fn operation(map: &Map<String, Value>, node_type: &str) -> String {
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    let mut name = match (node_type, text("Strategy")) {
        ("Aggregate", Some("Hashed")) => "HashAggregate".to_owned(),
        ("Aggregate", Some("Sorted")) => "GroupAggregate".to_owned(),
        ("Aggregate", Some("Mixed")) => "MixedAggregate".to_owned(),
        ("SetOp", Some("Hashed")) => "HashSetOp".to_owned(),
        _ => node_type.to_owned(),
    };
    if let Some(join) = text("Join Type").filter(|j| *j != "Inner") {
        if name == "Nested Loop" {
            name = format!("Nested Loop {join} Join");
        } else if let Some(kind) = name.strip_suffix(" Join") {
            name = format!("{kind} {join} Join");
        }
    }
    if text("Scan Direction") == Some("Backward") {
        name.push_str(" Backward");
    }
    if let Some(mode) = text("Partial Mode").filter(|m| *m != "Simple") {
        name = format!("{mode} {name}");
    }
    if map.get("Parallel Aware").and_then(Value::as_bool) == Some(true) {
        name = format!("Parallel {name}");
    }
    name
}

/// What the node reads, as a text plan names it.
fn target(map: &Map<String, Value>, node_type: &str) -> Option<String> {
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    let named = |name: &str| match text("Alias").filter(|a| *a != name) {
        Some(alias) => format!("{name} {alias}"),
        None => name.to_owned(),
    };
    let relation = text("Relation Name").map(|r| {
        let name = text("Schema").map_or_else(|| r.to_owned(), |s| format!("{s}.{r}"));
        match text("Alias").filter(|a| *a != r) {
            Some(alias) => format!("{name} {alias}"),
            None => name,
        }
    });
    if let Some(index) = text("Index Name") {
        return Some(match relation {
            Some(relation) => format!("using {index} on {relation}"),
            None => format!("on {index}"),
        });
    }
    if let Some(relation) = relation {
        return Some(format!("on {relation}"));
    }
    if let Some(cte) = text("CTE Name") {
        return Some(format!("on {}", named(cte)));
    }
    if let Some(function) = text("Function Name") {
        return Some(format!("on {}", named(function)));
    }
    match node_type {
        "Subquery Scan" | "Values Scan" | "WorkTable Scan" => {
            text("Alias").map(|a| format!("on {a}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS)` of a join with an aggregate,
    /// an index scan in a nested loop, and a part that never ran.
    const ANALYZED: &str = r#"[
  {
    "Plan": {
      "Node Type": "Aggregate",
      "Strategy": "Hashed",
      "Partial Mode": "Simple",
      "Parallel Aware": false,
      "Startup Cost": 120.50,
      "Total Cost": 122.50,
      "Plan Rows": 200,
      "Plan Width": 40,
      "Actual Startup Time": 4.512,
      "Actual Total Time": 4.601,
      "Actual Rows": 180,
      "Actual Loops": 1,
      "Group Key": ["u.name"],
      "Shared Hit Blocks": 12,
      "Shared Read Blocks": 3,
      "Plans": [
        {
          "Node Type": "Hash Join",
          "Parent Relationship": "Outer",
          "Parallel Aware": false,
          "Join Type": "Left",
          "Startup Cost": 30.00,
          "Total Cost": 110.00,
          "Plan Rows": 1000,
          "Plan Width": 36,
          "Actual Startup Time": 0.900,
          "Actual Total Time": 3.800,
          "Actual Rows": 1000,
          "Actual Loops": 1,
          "Hash Cond": "(o.user_id = u.id)",
          "Plans": [
            {
              "Node Type": "Seq Scan",
              "Parent Relationship": "Outer",
              "Parallel Aware": false,
              "Relation Name": "orders",
              "Alias": "o",
              "Startup Cost": 0.00,
              "Total Cost": 70.00,
              "Plan Rows": 1000,
              "Plan Width": 8,
              "Actual Startup Time": 0.010,
              "Actual Total Time": 2.500,
              "Actual Rows": 1000,
              "Actual Loops": 1,
              "Filter": "(total > 0)",
              "Rows Removed by Filter": 25
            },
            {
              "Node Type": "Hash",
              "Parent Relationship": "Inner",
              "Parallel Aware": false,
              "Startup Cost": 20.00,
              "Total Cost": 20.00,
              "Plan Rows": 400,
              "Plan Width": 36,
              "Actual Startup Time": 0.800,
              "Actual Total Time": 0.800,
              "Actual Rows": 400,
              "Actual Loops": 1,
              "Plans": [
                {
                  "Node Type": "Index Scan",
                  "Parent Relationship": "Outer",
                  "Parallel Aware": false,
                  "Scan Direction": "Forward",
                  "Index Name": "users_pkey",
                  "Relation Name": "users",
                  "Alias": "u",
                  "Startup Cost": 0.15,
                  "Total Cost": 20.00,
                  "Plan Rows": 400,
                  "Plan Width": 36,
                  "Actual Startup Time": 0.020,
                  "Actual Total Time": 0.010,
                  "Actual Rows": 1,
                  "Actual Loops": 40,
                  "Index Cond": "(id > 10)",
                  "Rows Removed by Filter": 0
                },
                {
                  "Node Type": "Seq Scan",
                  "Parent Relationship": "SubPlan",
                  "Subplan Name": "SubPlan 1",
                  "Parallel Aware": false,
                  "Relation Name": "flags",
                  "Alias": "flags",
                  "Startup Cost": 0.00,
                  "Total Cost": 0.00,
                  "Plan Rows": 1,
                  "Plan Width": 4,
                  "Actual Loops": 0
                }
              ]
            }
          ]
        }
      ]
    },
    "Planning": { "Shared Hit Blocks": 4 },
    "Planning Time": 0.215,
    "Triggers": [],
    "Execution Time": 4.702
  }
]"#;

    #[test]
    fn an_analyzed_plan_keeps_its_nodes_costs_times_and_conditions() {
        let plan = Plan::parse(ANALYZED).unwrap();
        assert!(plan.analyzed);
        assert_eq!(plan.planning_ms, Some(0.215));
        assert_eq!(plan.execution_ms, Some(4.702));
        assert_eq!(plan.node_count(), 6);
        let root = &plan.root;
        assert_eq!(root.operation, "HashAggregate");
        assert_eq!(root.estimate(), "(cost=120.50..122.50 rows=200 width=40)");
        assert_eq!(
            root.details,
            [
                ("Group Key".to_owned(), "u.name".to_owned()),
                ("Buffers".to_owned(), "shared hit=12 read=3".to_owned()),
            ]
        );
        let join = &root.children[0];
        assert_eq!(join.title(), "Hash Left Join");
        assert_eq!(
            join.details[0],
            ("Hash Cond".into(), "(o.user_id = u.id)".into())
        );
        let scan = &join.children[0];
        assert_eq!(scan.title(), "Seq Scan on orders o");
        assert_eq!(
            scan.details,
            [
                ("Filter".to_owned(), "(total > 0)".to_owned()),
                ("Rows Removed by Filter".to_owned(), "25".to_owned()),
            ]
        );
        let hash = &join.children[1];
        let index = &hash.children[0];
        assert_eq!(index.title(), "Index Scan using users_pkey on users u");
        assert_eq!(
            index.actual,
            Some(Actual {
                startup_ms: 0.02,
                total_ms: 0.01,
                rows: 1.,
                loops: 40.
            })
        );
        assert_eq!(
            index.details,
            [("Index Cond".to_owned(), "(id > 10)".to_owned())],
            "no zero counters"
        );
        let never = &hash.children[1];
        assert_eq!(never.subplan.as_deref(), Some("SubPlan 1"));
        assert_eq!(
            never.title(),
            "Seq Scan on flags",
            "an alias like the table is left out"
        );
        assert_eq!(never.actual.unwrap().loops, 0.);
        let ids: Vec<usize> = plan
            .rows(&HashSet::new())
            .iter()
            .map(|r| r.node.id)
            .collect();
        assert_eq!(ids, [0, 1, 2, 3, 4, 5], "reading order");
    }

    #[test]
    fn shares_are_each_nodes_own_part_and_collapsed_nodes_hide_their_children() {
        let plan = Plan::parse(ANALYZED).unwrap();
        let rows = plan.rows(&HashSet::new());
        let depths: Vec<usize> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, [0, 1, 2, 2, 3, 3]);
        let share = |id: usize| rows.iter().find(|r| r.node.id == id).unwrap().share;
        // By time: the scan of orders took 2.5 of the 4.601 ms.
        assert!((share(2) - 2.5 / 4.601).abs() < 1e-9);
        // The join: 3.8 less its children's 2.5 and 0.8.
        assert!((share(1) - 0.5 / 4.601).abs() < 1e-9);
        // The index scan ran 40 times: 0.4 ms in all, more than the Hash above it.
        assert!((share(4) - 0.4 / 4.601).abs() < 1e-9);
        assert!((share(3) - 0.4 / 4.601).abs() < 1e-9);
        let most = rows
            .iter()
            .max_by(|a, b| a.share.total_cmp(&b.share))
            .unwrap();
        assert_eq!(most.node.id, 2, "the most expensive node");
        let shown: Vec<usize> = plan
            .rows(&HashSet::from([1]))
            .iter()
            .map(|r| r.node.id)
            .collect();
        assert_eq!(shown, [0, 1]);
        let shown: Vec<usize> = plan
            .rows(&HashSet::from([3]))
            .iter()
            .map(|r| r.node.id)
            .collect();
        assert_eq!(shown, [0, 1, 2, 3]);
    }

    #[test]
    fn a_plain_plan_shares_by_cost_and_reads_as_explain_text() {
        let json = r#"[{"Plan": {
            "Node Type": "Nested Loop", "Join Type": "Anti", "Parallel Aware": false,
            "Startup Cost": 0.29, "Total Cost": 100.00, "Plan Rows": 10, "Plan Width": 4,
            "Plans": [
              {"Node Type": "Seq Scan", "Parent Relationship": "Outer", "Parallel Aware": true,
               "Relation Name": "a", "Alias": "a", "Startup Cost": 0.00, "Total Cost": 25.00,
               "Plan Rows": 10, "Plan Width": 4},
              {"Node Type": "Index Only Scan", "Parent Relationship": "Inner", "Parallel Aware": false,
               "Scan Direction": "Backward", "Index Name": "b_idx", "Relation Name": "b", "Alias": "b",
               "Startup Cost": 0.29, "Total Cost": 7.00, "Plan Rows": 1, "Plan Width": 4,
               "Index Cond": "(id = a.id)"}
            ]}, "Planning Time": 0.1}]"#;
        let plan = Plan::parse(json).unwrap();
        assert!(!plan.analyzed);
        let rows = plan.rows(&HashSet::new());
        assert!((rows[0].share - 0.68).abs() < 1e-9, "100 less 25 and 7");
        assert!((rows[1].share - 0.25).abs() < 1e-9);
        assert_eq!(
            plan.text(),
            "Nested Loop Anti Join  (cost=0.29..100.00 rows=10 width=4)\n\
             \x20 ->  Parallel Seq Scan on a  (cost=0.00..25.00 rows=10 width=4)\n\
             \x20 ->  Index Only Scan Backward using b_idx on b  (cost=0.29..7.00 rows=1 width=4)\n\
             \x20       Index Cond: (id = a.id)\n\
             Planning Time: 0.100 ms"
        );
        let analyzed = Plan::parse(ANALYZED).unwrap().text();
        assert!(analyzed.contains("  ->  Hash Left Join  (cost=30.00..110.00 rows=1000 width=36) (actual time=0.900..3.800 rows=1000 loops=1)\n        Hash Cond: (o.user_id = u.id)\n"), "{analyzed}");
        assert!(
            analyzed
                .contains("Seq Scan on flags  (cost=0.00..0.00 rows=1 width=4) (never executed)"),
            "{analyzed}"
        );
        assert!(analyzed.ends_with("Execution Time: 4.702 ms"));
    }

    #[test]
    fn what_is_not_a_plan_is_an_error() {
        assert!(Plan::parse("not json").is_err());
        assert!(Plan::parse("[]").is_err());
        assert!(Plan::parse(r#"[{"Planning Time": 1}]"#).is_err());
    }
}
