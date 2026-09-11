//! The index poster (spec 4.4): a small card drawn from the plan model, so
//! the index can show what an artifact looks like without a browser at
//! generation time. A pure function of the plan and the review's state:
//! title, kind badge, a bar per phase sized by task count with its done
//! share filled, a marker per risk, and one line of review state. Pinned by
//! a golden fixture the way the dependency graph is, so drift is caught.
//!
//! Class names carry an `ap-` prefix and the card brings its own `<style>`:
//! a standalone `.svg` file looks right on its own, and inlined into the
//! served index page the same classes let the page restyle it for a dark
//! theme without touching the file.

use crate::plan::model::{Plan, RiskLevel, Status};
use crate::plan::svg::{esc, wrap_title};

pub const WIDTH: i64 = 320;
pub const HEIGHT: i64 = 180;
const PAD: i64 = 16;
const BAR_GAP: i64 = 3;
const BAR_MIN: i64 = 12;
const RISKS_SHOWN: usize = 16;

/// What the review has come to, for the card's last line. The default is a
/// plan that was rendered and never pushed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewState {
    pub revision: u32,
    pub open_threads: usize,
    pub unanchored_threads: usize,
    pub submitted: bool,
    pub verdict: Option<String>,
}

const STYLE: &str = "\
.ap-bg{fill:#f7f4ef;stroke:#ddd6c9}\
.ap-kind{fill:#9b3d20}\
.ap-kind-text{fill:#f7f4ef;font:600 8px \"JetBrains Mono\",ui-monospace,Menlo,monospace;letter-spacing:.14em}\
.ap-rev,.ap-counts,.ap-state{fill:#6b6558;font:400 10px \"JetBrains Mono\",ui-monospace,Menlo,monospace}\
.ap-title{fill:#1c1a15;font:500 18px Newsreader,Georgia,\"Times New Roman\",serif}\
.ap-phase{fill:#ddd6c9}\
.ap-phase-done{fill:#2f6b3d}\
.ap-risk-high{fill:#9b3d20}\
.ap-risk-medium{fill:#7a5c2f}\
.ap-risk-low{fill:#2f6b3d}";

pub fn poster_svg(plan: &Plan, state: &ReviewState) -> String {
    let title = &plan.meta.title;
    let tasks: usize = plan.phases.iter().map(|p| p.tasks.len()).sum();
    let mut out = String::with_capacity(4096);
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {HEIGHT}\" \
         width=\"{WIDTH}\" height=\"{HEIGHT}\" class=\"ap\" role=\"img\" \
         aria-labelledby=\"ap-title\">"
    ));
    out.push_str(&format!("<title id=\"ap-title\">{}</title>", esc(title)));
    out.push_str(&format!("<style>{STYLE}</style>"));
    out.push_str(&format!(
        "<rect class=\"ap-bg\" x=\"0.5\" y=\"0.5\" width=\"{}\" height=\"{}\" rx=\"6\"/>",
        WIDTH - 1,
        HEIGHT - 1
    ));

    // Kind badge, top left; revision, top right.
    out.push_str(&format!(
        "<rect class=\"ap-kind\" x=\"{PAD}\" y=\"14\" width=\"34\" height=\"16\" rx=\"3\"/>\
         <text class=\"ap-kind-text\" x=\"{}\" y=\"25.5\" text-anchor=\"middle\">PLAN</text>",
        PAD + 17
    ));
    let rev = if state.revision == 0 {
        "not pushed".to_string()
    } else {
        format!("rev {}", state.revision)
    };
    out.push_str(&format!(
        "<text class=\"ap-rev\" x=\"{}\" y=\"26\" text-anchor=\"end\">{}</text>",
        WIDTH - PAD,
        esc(&rev)
    ));

    // Title, at most two lines.
    for (i, line) in wrap_title(title, 30).iter().enumerate() {
        out.push_str(&format!(
            "<text class=\"ap-title\" x=\"{PAD}\" y=\"{}\">{}</text>",
            60 + i as i64 * 22,
            esc(line)
        ));
    }

    // Counts.
    let mut counts = format!(
        "{} {} · {} {}",
        plan.phases.len(),
        plural(plan.phases.len(), "phase"),
        tasks,
        plural(tasks, "task")
    );
    if !plan.open_questions.is_empty() {
        counts.push_str(&format!(
            " · {} {}",
            plan.open_questions.len(),
            plural(plan.open_questions.len(), "question")
        ));
    }
    out.push_str(&format!(
        "<text class=\"ap-counts\" x=\"{PAD}\" y=\"108\">{}</text>",
        esc(&counts)
    ));

    // One bar per phase, sized by task count, its done share filled.
    let n = plan.phases.len() as i64;
    if n > 0 {
        let available = (WIDTH - 2 * PAD) - BAR_GAP * (n - 1);
        let total = tasks.max(1) as i64;
        let mut widths: Vec<i64> = plan
            .phases
            .iter()
            .map(|p| {
                let share = if tasks == 0 {
                    available / n
                } else {
                    available * p.tasks.len() as i64 / total
                };
                share.max(BAR_MIN)
            })
            .collect();
        // Minimums can push the row past the edge; take it back from the
        // widest bars, which are the ones with room to give.
        let mut over: i64 = widths.iter().sum::<i64>() - available;
        while over > 0 {
            let widest = (0..widths.len())
                .max_by_key(|&i| (widths[i], std::cmp::Reverse(i)))
                .expect("n > 0");
            widths[widest] -= 1;
            over -= 1;
        }
        let mut x = PAD;
        for (phase, w) in plan.phases.iter().zip(widths) {
            let done = phase
                .tasks
                .iter()
                .filter(|t| matches!(t.status, Status::Done))
                .count();
            let done_w = if phase.tasks.is_empty() {
                0
            } else {
                w * done as i64 / phase.tasks.len() as i64
            };
            out.push_str(&format!(
                "<g><title>{}: {} {}, {} done</title>\
                 <rect class=\"ap-phase\" x=\"{x}\" y=\"120\" width=\"{w}\" height=\"10\" rx=\"2\"/>",
                esc(&phase.title),
                phase.tasks.len(),
                plural(phase.tasks.len(), "task"),
                done
            ));
            if done_w > 0 {
                out.push_str(&format!(
                    "<rect class=\"ap-phase-done\" x=\"{x}\" y=\"120\" width=\"{done_w}\" height=\"10\" rx=\"2\"/>"
                ));
            }
            out.push_str("</g>");
            x += w + BAR_GAP;
        }
    }

    // A marker per risk, bottom left.
    for (i, risk) in plan.risks.iter().take(RISKS_SHOWN).enumerate() {
        let class = match risk.severity {
            RiskLevel::High => "ap-risk-high",
            RiskLevel::Medium => "ap-risk-medium",
            RiskLevel::Low => "ap-risk-low",
        };
        out.push_str(&format!(
            "<circle class=\"{class}\" cx=\"{}\" cy=\"156\" r=\"4\"><title>{}</title></circle>",
            PAD + 4 + i as i64 * 12,
            esc(&risk.title)
        ));
    }
    if plan.risks.len() > RISKS_SHOWN {
        out.push_str(&format!(
            "<text class=\"ap-counts\" x=\"{}\" y=\"160\">+{}</text>",
            PAD + 4 + RISKS_SHOWN as i64 * 12,
            plan.risks.len() - RISKS_SHOWN
        ));
    }

    // The review, bottom right.
    let state_line = if state.revision == 0 {
        String::new()
    } else {
        let mut parts = vec![format!("{} open", state.open_threads)];
        if state.unanchored_threads > 0 {
            parts.push(format!("{} unanchored", state.unanchored_threads));
        }
        parts.push(
            match state.verdict.as_deref() {
                Some("approve") => "approved",
                Some("request_changes") => "changes requested",
                Some("comment") => "commented",
                Some(other) => other,
                None => "in review",
            }
            .to_string(),
        );
        parts.join(" · ")
    };
    if !state_line.is_empty() {
        out.push_str(&format!(
            "<text class=\"ap-state\" x=\"{}\" y=\"160\" text-anchor=\"end\">{}</text>",
            WIDTH - PAD,
            esc(&state_line)
        ));
    }
    out.push_str("</svg>");
    out
}

fn plural(n: usize, unit: &str) -> String {
    if n == 1 {
        unit.to_string()
    } else {
        format!("{unit}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::model::parse;

    fn fixture(name: &str) -> Plan {
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/plan/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        parse(&raw, false).unwrap().plan
    }

    fn reviewed() -> ReviewState {
        ReviewState {
            revision: 3,
            open_threads: 2,
            unanchored_threads: 1,
            submitted: true,
            verdict: Some("request_changes".to_string()),
        }
    }

    #[test]
    fn golden_poster() {
        let path = format!(
            "{}/tests/fixtures/plan/kitchen-sink-poster.svg",
            env!("CARGO_MANIFEST_DIR")
        );
        let got = poster_svg(&fixture("kitchen-sink.json"), &reviewed());
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write(&path, &got).unwrap();
        } else {
            let expected = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                got, expected,
                "golden drift — rerun with UPDATE_GOLDEN=1 if intended"
            );
        }
    }

    #[test]
    fn the_poster_is_deterministic_and_carries_every_fact() {
        let plan = fixture("kitchen-sink.json");
        let a = poster_svg(&plan, &reviewed());
        assert_eq!(a, poster_svg(&plan, &reviewed()));
        assert!(a.starts_with("<svg"));
        assert!(a.contains("<title id=\"ap-title\">Auth refactor</title>"));
        assert!(a.contains(">PLAN<"));
        assert!(a.contains(">rev 3<"));
        assert!(a.contains("2 phases · 5 tasks · 2 questions"));
        assert_eq!(
            a.matches("class=\"ap-phase\"").count(),
            2,
            "a bar per phase"
        );
        assert_eq!(
            a.matches("class=\"ap-phase-done\"").count(),
            1,
            "only Core has a done task"
        );
        assert!(a.contains("class=\"ap-risk-high\""), "the one risk is high");
        assert!(a.contains("2 open · 1 unanchored · changes requested"));
    }

    #[test]
    fn an_unpushed_plan_says_so_and_has_no_review_line() {
        let svg = poster_svg(&fixture("minimal.json"), &ReviewState::default());
        assert!(svg.contains(">not pushed<"));
        assert!(!svg.contains("class=\"ap-state\""));
        assert!(svg.contains("1 phase · 1 task<"));
    }

    #[test]
    fn a_hostile_title_is_text_not_markup() {
        let mut plan = fixture("minimal.json");
        plan.meta.title = "<script>alert(1)</script> & \"co\"".to_string();
        plan.phases[0].title = "</title><b>".to_string();
        let svg = poster_svg(&plan, &ReviewState::default());
        assert!(!svg.contains("<script"), "{svg}");
        assert!(!svg.contains("<b>"), "{svg}");
        assert!(svg.contains("&lt;script&gt;"), "{svg}");
    }

    #[test]
    fn the_bars_always_fit_the_card() {
        let mut plan = fixture("minimal.json");
        // Thirty phases with one task each would need 30 × 12 + 29 × 3 =
        // 447 px at the minimum width; the row must still end inside the
        // card, so minimums give way.
        let one = plan.phases[0].clone();
        plan.phases = (0..30)
            .map(|i| {
                let mut p = one.clone();
                p.id = format!("p-{i}");
                p
            })
            .collect();
        let svg = poster_svg(&plan, &ReviewState::default());
        let mut right_edge = 0i64;
        for piece in svg.split("<rect class=\"ap-phase\" ").skip(1) {
            let x: i64 = piece
                .split("x=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let w: i64 = piece
                .split("width=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            right_edge = right_edge.max(x + w);
        }
        assert!(right_edge <= WIDTH - PAD, "{right_edge}");
        assert_eq!(svg.matches("class=\"ap-phase\"").count(), 30);
    }
}
