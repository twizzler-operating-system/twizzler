//! Renders the startup banner printed once at launch.

use super::Agent;
use crate::helpers::middle;

pub fn build_welcome(agent: &Agent, model: &str, provider: &str) -> String {
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(80)
        .clamp(68, 84);
    let inner = width - 4;
    let gap = 3;
    let left_width = (inner - gap) / 2;
    let right_width = inner - gap - left_width;
    let row = |text: &str| {
        let body = middle(text, width - 4);
        format!("| {:<width$} |", body, width = width - 4)
    };
    let divider = |ch: char| format!("+{}+", ch.to_string().repeat(width - 2));
    let center = |text: &str| {
        let body = middle(text, inner);
        format!("| {:^inner$} |", body, inner = inner)
    };
    let cell = |label: &str, value: &str, size: usize| {
        let body = middle(format!("{label:<9} {value}"), size);
        format!("{body:<size$}")
    };
    let pair = |left_label: &str, left_value: &str, right_label: &str, right_value: &str| {
        let left = cell(left_label, left_value, left_width);
        let right = cell(right_label, right_value, right_width);
        format!("| {left}{}{right} |", " ".repeat(gap))
    };
    let mut rows = vec![
        divider('='),
        center("    ______"),
        center("   /\\     \\"),
        center("  /  \\_____\\"),
        center("  \\  /     /"),
        center("   \\/_____/"),
        center("TWIZBOX"),
        divider('-'),
        row(""),
        row(&format!(
            "WORKSPACE  {}",
            middle(agent.workspace().cwd.display(), inner - 11)
        )),
        pair("MODEL", model, "PROVIDER", provider),
        pair(
            "APPROVAL",
            agent.approval_policy().as_str(),
            "SESSION",
            agent.session_id(),
        ),
        row(""),
    ];
    rows.push(divider('='));
    rows.join("\n")
}
