//! A PostgreSQL plan in a console's result area, as a tree with columns:
//! each step's cost, rows and (with Analyze) measured time, the expensive
//! ones marked by their own share of the plan. "Text" shows it as `EXPLAIN`
//! writes it.
use super::*;
use monocode_core::db::Plan;
use monocode_core::db::plan::count;

/// A plan a console shows, and the steps folded away.
pub(super) struct PlanView {
    pub plan: Arc<Plan>,
    pub collapsed: HashSet<usize>,
}

impl PlanView {
    pub fn new(plan: Plan) -> Self {
        Self {
            plan: Arc::new(plan),
            collapsed: HashSet::new(),
        }
    }

    /// The tree's rows as they are drawn.
    pub fn lines(&self) -> Vec<PlanLine> {
        self.plan
            .rows(&self.collapsed)
            .into_iter()
            .map(|row| {
                let n = row.node;
                let actual = n.actual.map(|a| {
                    if a.loops == 0. {
                        ("never executed".to_owned(), String::new(), String::new())
                    } else {
                        (
                            format!("{:.3}..{:.3}", a.startup_ms, a.total_ms),
                            count(a.rows),
                            count(a.loops),
                        )
                    }
                });
                PlanLine {
                    id: n.id,
                    depth: row.depth,
                    title: n.title(),
                    subplan: n.subplan.clone(),
                    details: n
                        .details
                        .iter()
                        .map(|(label, value)| format!("{label}: {value}"))
                        .collect::<Vec<_>>()
                        .join(" · "),
                    cost: format!("{:.2}..{:.2}", n.startup_cost, n.total_cost),
                    rows: count(n.plan_rows),
                    width: n.plan_width.to_string(),
                    actual,
                    share: row.share,
                    folds: (!n.children.is_empty()).then(|| !self.collapsed.contains(&n.id)),
                }
            })
            .collect()
    }
}

/// One drawn row of a plan.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanLine {
    pub id: usize,
    pub depth: usize,
    pub title: String,
    pub subplan: Option<String>,
    pub details: String,
    pub cost: String,
    pub rows: String,
    pub width: String,
    /// Time, rows and loops, with Analyze.
    pub actual: Option<(String, String, String)>,
    pub share: f64,
    /// Whether it is open, when it has steps under it.
    pub folds: Option<bool>,
}

/// How a step's share is marked: red from half the plan, orange from a fifth.
pub(super) fn heat(share: f64) -> Option<u32> {
    if share >= 0.5 {
        Some(0xf87171)
    } else if share >= 0.2 {
        Some(0xfb923c)
    } else {
        None
    }
}

const COST_W: f32 = 120.;
const NUM_W: f32 = 64.;
const TIME_W: f32 = 120.;
const SHARE_W: f32 = 90.;

fn cell(width: f32, text: String) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .px(px(6.))
        .flex()
        .justify_end()
        .truncate()
        .child(text)
}

impl DatabaseView {
    /// Folds or unfolds step `node` of the plan console `ix` shows.
    pub(super) fn toggle_plan_node(&mut self, ix: usize, node: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.consoles.get_mut(ix).and_then(|c| c.plan.as_mut()) {
            if !view.collapsed.remove(&node) {
                view.collapsed.insert(node);
            }
            cx.notify();
        }
    }

    pub(super) fn plan_tree(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = self.consoles[ix].plan.as_ref() else {
            return div().into_any_element();
        };
        let plan = view.plan.clone();
        let lines = view.lines();
        let analyzed = plan.analyzed;
        let timing = [
            plan.planning_ms.map(|ms| format!("Planning {ms:.3} ms")),
            plan.execution_ms.map(|ms| format!("Execution {ms:.3} ms")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let header = |label: &'static str, width: Option<f32>| match width {
            Some(w) => cell(w, label.to_owned()).into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .px(px(6.))
                .child(label)
                .into_any_element(),
        };
        let mut columns = vec![
            header("Operation", None),
            header("Cost", Some(COST_W)),
            header("Rows", Some(NUM_W)),
            header("Width", Some(NUM_W)),
        ];
        if analyzed {
            columns.extend([
                header("Actual time (ms)", Some(TIME_W)),
                header("Actual rows", Some(NUM_W + 16.)),
                header("Loops", Some(NUM_W)),
            ]);
        }
        columns.push(header(
            if analyzed { "Time share" } else { "Cost share" },
            Some(SHARE_W),
        ));
        let text = plan.text();
        let title = if analyzed { "Plan (analyzed)" } else { "Plan" };
        let bar = div()
            .h(px(28.))
            .flex_shrink_0()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(content(0.07))
            .text_size(px(11.5))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(content(0.85))
                    .child(title),
            )
            .child(div().text_color(content(0.45)).child(timing))
            .child(div().flex_1())
            .child(div().debug_selector(|| "database-plan-text".into()).child(
                crate::project_settings::button("database-plan-text", "Text").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.preview = Some((title.to_owned(), text.clone()));
                        cx.notify();
                    }),
                ),
            ))
            .child(
                icon_button("database-plan-close", "Close the plan", "x")
                    .size(px(22.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(console) = this.consoles.get_mut(ix) {
                            console.plan = None;
                        }
                        cx.notify();
                    })),
            );
        let rows = lines.into_iter().map(|line| {
            let hot = heat(line.share);
            let id = line.id;
            let mut row = div()
                .id(("database-plan-row", id))
                .debug_selector(move || format!("database-plan-row-{id}"))
                .min_h(px(24.))
                .py(px(3.))
                .flex()
                .items_start()
                .text_size(px(12.))
                .font_family("monospace")
                .text_color(content(0.85))
                .border_b_1()
                .border_color(content(0.04))
                .hover(|s| s.bg(content(0.05)))
                .when_some(hot, |d, hex| d.bg(gpui::Hsla::from(rgb(hex)).opacity(0.1)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(px(6. + 16. * line.depth as f32))
                        .pr(px(6.))
                        .flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .id(("database-plan-toggle", id))
                                .debug_selector(move || format!("database-plan-toggle-{id}"))
                                .w(px(14.))
                                .h(px(16.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .when_some(line.folds, |d, open| {
                                    d.cursor_pointer()
                                        .child(
                                            icon(
                                                if open {
                                                    "chevron-down"
                                                } else {
                                                    "chevron-right"
                                                },
                                                12.,
                                            )
                                            .text_color(content(0.45)),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_plan_node(ix, id, cx)
                                        }))
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(6.))
                                        .when_some(line.subplan.clone(), |d, name| {
                                            d.child(div().text_color(content(0.45)).child(name))
                                        })
                                        .child(
                                            div()
                                                .when_some(hot, |d, hex| {
                                                    d.text_color(rgb(hex))
                                                        .font_weight(FontWeight::MEDIUM)
                                                })
                                                .child(line.title.clone()),
                                        ),
                                )
                                .when(!line.details.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(content(0.5))
                                            .child(line.details.clone()),
                                    )
                                }),
                        ),
                )
                .child(cell(COST_W, line.cost.clone()))
                .child(cell(NUM_W, line.rows.clone()))
                .child(cell(NUM_W, line.width.clone()));
            if analyzed {
                let (time, rows, loops) = line.actual.clone().unwrap_or_default();
                row = row
                    .child(cell(TIME_W, time))
                    .child(cell(NUM_W + 16., rows))
                    .child(cell(NUM_W, loops));
            }
            row.child(
                div()
                    .w(px(SHARE_W))
                    .flex_shrink_0()
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .h(px(6.))
                            .w(px(40.))
                            .rounded(px(3.))
                            .bg(content(0.08))
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(3.))
                                    .w(px(40. * line.share as f32))
                                    .bg(hot.map_or(content(0.35), |hex| rgb(hex).into())),
                            ),
                    )
                    .child(format!("{:.0}%", line.share * 100.)),
            )
            .into_any_element()
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(
                div()
                    .h(px(24.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(content(0.07))
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .children(columns),
            )
            .child(
                div()
                    .id("database-plan")
                    .debug_selector(|| "database-plan".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"[{"Plan": {
        "Node Type": "Hash Join", "Join Type": "Inner", "Parallel Aware": false,
        "Startup Cost": 1.09, "Total Cost": 30.00, "Plan Rows": 4, "Plan Width": 72,
        "Actual Startup Time": 0.03, "Actual Total Time": 1.00, "Actual Rows": 4, "Actual Loops": 1,
        "Hash Cond": "(o.user_id = u.id)",
        "Plans": [
          {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o",
           "Startup Cost": 0.0, "Total Cost": 25.0, "Plan Rows": 4, "Plan Width": 40,
           "Actual Startup Time": 0.01, "Actual Total Time": 0.70, "Actual Rows": 4, "Actual Loops": 1,
           "Filter": "(total > 0)", "Rows Removed by Filter": 2},
          {"Node Type": "Hash", "Startup Cost": 1.05, "Total Cost": 1.05, "Plan Rows": 3, "Plan Width": 36,
           "Actual Startup Time": 0.1, "Actual Total Time": 0.1, "Actual Rows": 3, "Actual Loops": 1,
           "Plans": [
             {"Node Type": "Seq Scan", "Relation Name": "users", "Alias": "u",
              "Startup Cost": 0.0, "Total Cost": 1.03, "Plan Rows": 3, "Plan Width": 36,
              "Actual Startup Time": 0.005, "Actual Total Time": 0.05, "Actual Rows": 3, "Actual Loops": 1}
           ]}
        ]}, "Planning Time": 0.2, "Execution Time": 1.1}]"#;

    #[test]
    fn plan_lines_carry_the_columns_and_fold() {
        let mut view = PlanView::new(Plan::parse(PLAN).unwrap());
        let lines = view.lines();
        let titles: Vec<(usize, &str)> =
            lines.iter().map(|l| (l.depth, l.title.as_str())).collect();
        assert_eq!(
            titles,
            [
                (0, "Hash Join"),
                (1, "Seq Scan on orders o"),
                (1, "Hash"),
                (2, "Seq Scan on users u"),
            ]
        );
        let scan = &lines[1];
        assert_eq!(scan.cost, "0.00..25.00");
        assert_eq!((scan.rows.as_str(), scan.width.as_str()), ("4", "40"));
        assert_eq!(
            scan.actual,
            Some(("0.010..0.700".into(), "4".into(), "1".into()))
        );
        assert_eq!(
            scan.details,
            "Filter: (total > 0) · Rows Removed by Filter: 2"
        );
        assert_eq!(heat(scan.share), Some(0xf87171), "0.7 of 1 ms");
        assert_eq!(
            heat(lines[0].share),
            Some(0xfb923c),
            "the join's own 0.2 ms"
        );
        assert_eq!(heat(lines[3].share), None);
        assert_eq!(lines[0].folds, Some(true));
        assert_eq!(lines[1].folds, None, "a leaf does not fold");
        view.collapsed.insert(2);
        let lines = view.lines();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].folds, Some(false));
    }

    fn open(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::WindowHandle<DatabaseView>, tempfile::TempDir) {
        cx.update(input::bind);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), move |_, cx| {
                cx.new(|cx| DatabaseView::new(path, cx))
            })
            .unwrap()
        });
        (window, dir)
    }

    #[gpui::test]
    fn the_result_area_draws_the_plan_tree_and_folds_a_step(cx: &mut gpui::TestAppContext) {
        let (window, _dir) = open(cx);
        window
            .update(cx, |view, _, cx| {
                let mut source = DataSource::new("a");
                source.host = "127.0.0.1".into();
                view.save_source(source, None, cx);
                view.consoles[0].plan = Some(PlanView::new(Plan::parse(PLAN).unwrap()));
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("database-plan").is_some(),
            "the tree replaces the grid"
        );
        assert!(visual.debug_bounds("database-grid").is_none());
        const ROWS: [&str; 4] = [
            "database-plan-row-0",
            "database-plan-row-1",
            "database-plan-row-2",
            "database-plan-row-3",
        ];
        for id in ROWS {
            assert!(visual.debug_bounds(id).is_some(), "{id}");
        }
        let (one, two) = (
            visual.debug_bounds(ROWS[1]).unwrap(),
            visual.debug_bounds(ROWS[2]).unwrap(),
        );
        assert!(one.origin.y < two.origin.y, "in reading order");
        let toggle = visual.debug_bounds("database-plan-toggle-2").unwrap();
        visual.simulate_click(toggle.center(), Default::default());
        visual.run_until_parked();
        visual.refresh().unwrap();
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("database-plan-row-3").is_none(),
            "folded away"
        );
        assert!(visual.debug_bounds("database-plan-row-2").is_some());
        // The text view is a click away.
        let text = visual.debug_bounds("database-plan-text").unwrap();
        visual.simulate_click(text.center(), Default::default());
        visual.run_until_parked();
        let (title, text) = window
            .read_with(visual, |v, _| v.preview.clone().expect("the text plan"))
            .unwrap();
        assert_eq!(title, "Plan (analyzed)");
        assert!(
            text.starts_with("Hash Join  (cost=1.09..30.00 rows=4 width=72)"),
            "{text}"
        );
    }
}
