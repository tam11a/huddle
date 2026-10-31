// Drawing the board: a btop-style dashboard with a little motion.
// Colours are truecolor (Catppuccin Mocha); NO_COLOR turns them off.
use crate::board::{STATUSES, Task, fmt_id, fmt_minutes};
use crate::time::{fmt_day, fmt_hm, fmt_when, hour, last_days, next_day, start_of_day};
use crate::tui::{Action, Board, PRIORITY, Prompt, State, listed, selected, shown};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Bar, BarChart, BarGroup, Block, BorderType, Chart, Clear, Dataset, Gauge, GraphType, Padding, Paragraph, Wrap};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type Rgb = (u8, u8, u8);
const TEXT: Rgb = (205, 214, 244);
const DIM: Rgb = (127, 132, 156);
const SUBTLE: Rgb = (69, 71, 90);
const SURFACE: Rgb = (49, 50, 68);
const BASE: Rgb = (30, 30, 46);
const RED: Rgb = (243, 139, 168);
const PEACH: Rgb = (250, 179, 135);
const YELLOW: Rgb = (249, 226, 175);
const GREEN: Rgb = (166, 227, 161);
const TEAL: Rgb = (148, 226, 213);
const BLUE: Rgb = (137, 180, 250);
const MAUVE: Rgb = (203, 166, 247);
const PINK: Rgb = (245, 194, 231);
const OVERLAY: Rgb = (147, 153, 178);
/// One colour per status, in STATUSES order.
const COLUMN: [Rgb; 6] = [OVERLAY, BLUE, YELLOW, MAUVE, RED, GREEN];
const PRIO: [Rgb; 4] = [RED, PEACH, BLUE, DIM];
const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const INTRO: Duration = Duration::from_millis(1400);
const FLASH: Duration = Duration::from_millis(700);
const TOAST: Duration = Duration::from_secs(4);
const POP: Duration = Duration::from_millis(150);
const FRAME: Duration = Duration::from_millis(33);
const REPORT: Duration = Duration::from_millis(900);

pub struct View<'a> {
    pub now: i64,
    pub t: Instant,
    pub file: &'a str,
}

fn no_color() -> bool {
    static NO: OnceLock<bool> = OnceLock::new();
    *NO.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

fn c(rgb: Rgb) -> Color {
    if no_color() { Color::Reset } else { Color::Rgb(rgb.0, rgb.1, rgb.2) }
}

fn fg(rgb: Rgb) -> Style {
    Style::new().fg(c(rgb))
}

fn bold(rgb: Rgb) -> Style {
    fg(rgb).add_modifier(Modifier::BOLD)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn gradient(text: &str, from: Rgb, to: Rgb) -> Vec<Span<'static>> {
    let n = (text.chars().count().max(2) - 1) as f64;
    text.chars().enumerate().map(|(i, ch)| Span::styled(ch.to_string(), bold(mix(from, to, i as f64 / n)))).collect()
}

fn chip(text: &str, color: Rgb) -> Span<'static> {
    let style = if no_color() { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new().fg(c(BASE)).bg(c(color)).add_modifier(Modifier::BOLD) };
    Span::styled(format!(" {text} "), style)
}

/// 0 → 1 over `span`, starting at `since`.
fn progress(t: Instant, since: Instant, span: Duration) -> f64 {
    (t.saturating_duration_since(since).as_secs_f64() / span.as_secs_f64()).min(1.0)
}

fn clip(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.into() } else if w == 0 { String::new() } else { s.chars().take(w - 1).chain(['…']).collect() }
}

fn panel(title: String, color: Rgb) -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded).border_style(fg(SUBTLE)).title_top(Span::styled(title, bold(color)))
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// How long the loop may sleep before the next frame: short while something moves.
pub fn next_frame(s: &State, t: Instant) -> Duration {
    let moving = s.intro.is_some()
        || s.flash.is_some_and(|(_, at)| progress(t, at, FLASH) < 1.0)
        || (!s.msg.is_empty() && progress(t, s.toast, TOAST) < 1.0)
        || (s.gauge - s.gauge_to).abs() > 0.002
        || s.grow < 1.0
        || s.report.as_ref().is_some_and(|(_, at)| progress(t, *at, REPORT) < 1.0)
        || s.sprint_view.as_ref().is_some_and(|(_, at)| progress(t, *at, REPORT) < 1.0)
        || s.prompt.as_ref().is_some_and(|p| progress(t, p.opened, POP) < 1.0);
    if moving { FRAME } else if s.prompt.is_some() { Duration::from_millis(530) } else if s.focus.is_some() { Duration::from_secs(1) } else { Duration::from_secs(60) }
}

pub fn draw(f: &mut Frame, s: &mut State, b: &Board, v: &View) {
    let area = f.area();
    if let Some(start) = s.intro {
        if v.t.saturating_duration_since(start) < INTRO {
            return intro(f, area, v.t.saturating_duration_since(start), v.now);
        }
        s.intro = None;
    }
    let (done, doing) = (b.cols[5].len(), b.cols[2].len());
    s.gauge_to = if done + doing == 0 { 0.0 } else { done as f64 / (done + doing) as f64 };
    s.gauge = if (s.gauge_to - s.gauge).abs() < 0.002 { s.gauge_to } else { s.gauge + (s.gauge_to - s.gauge) * 0.2 };
    s.grow = (s.grow + 0.07).min(1.0);

    let roomy = area.height >= 24 && area.width >= 90;
    let [head, body, info, foot] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1), Constraint::Length(if roomy { 9 } else { 0 }), Constraint::Length(1)]).areas(area);
    let cols = shown(b, s);
    let sel = selected(s, &cols);
    header(f, head, s, b, v);
    if s.list {
        list_view(f, body, s, &cols, v, sel.as_ref().map(|t| t.id));
    } else {
        columns(f, body, s, &cols, v, sel.as_ref().map(|t| t.id));
    }
    if roomy {
        let [detail, chart, projects] = Layout::horizontal([Constraint::Fill(3), Constraint::Fill(3), Constraint::Fill(2)]).areas(info);
        task_panel(f, detail, sel.as_ref(), v.now);
        chart_panel(f, chart, s.grow, b, v.now);
        project_panel(f, projects, b);
    }
    footer(f, foot, s, v.t);
    if s.help {
        help(f, area);
    }
    if let Some(sd) = &s.standup {
        standup(f, area, sd);
    }
    if let Some((r, at)) = &s.report {
        report(f, area, b, r, *at, v);
    }
    if let Some((sp, at)) = &s.sprint_view {
        sprint_screen(f, area, sp, *at, v.t);
    }
    if let Some(p) = &s.prompt {
        prompt(f, area, p, v.t);
    }
}

fn header(f: &mut Frame, area: Rect, s: &State, b: &Board, v: &View) {
    let today = start_of_day(v.now);
    let tomorrow = next_day(today);
    let open: Vec<&Task> = b.cols.iter().flatten().filter(|t| t.status != "done").collect();
    let overdue = open.iter().filter(|t| t.due_at.is_some_and(|d| d < today)).count();
    let due = open.iter().filter(|t| t.due_at.is_some_and(|d| d >= today && d < tomorrow)).count();
    let (done, doing) = (b.cols[5].len(), b.cols[2].len());
    let live = if next_frame(s, v.t) == FRAME { SPIN[(v.t.saturating_duration_since(s.started).as_millis() / 80 % 10) as usize] } else { "●" };

    let block = Block::bordered().border_type(BorderType::Rounded).border_style(fg(SUBTLE))
        .title_top(Line::from([vec![Span::raw(" ")], gradient("◆ huddle", PINK, BLUE), vec![Span::raw(" ")]].concat()))
        .title_top(Line::from(vec![Span::styled(format!(" {live} live "), fg(GREEN)), Span::styled(format!("· {} ", fmt_when(v.now)), fg(DIM))]).right_aligned());
    let mut filters = vec![];
    let typing = s.prompt.as_ref().filter(|p| matches!(p.action, Action::Search)).map(|p| p.value.as_str());
    if let Some(text) = typing.or((!s.filter.is_empty()).then_some(s.filter.as_str())) {
        filters.push(Span::styled(format!(" / {text} "), bold(PINK)));
    }
    if let Some(p) = &s.project {
        filters.push(Span::styled(format!(" #{p} "), bold(TEAL)));
    }
    if s.list {
        filters.push(Span::styled(" list view ", bold(BLUE)));
    }
    let block = if filters.is_empty() { block } else {
        filters.push(Span::styled("esc clears ", fg(DIM)));
        block.title_bottom(Line::from(filters).right_aligned())
    };
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(30)]).spacing(2).areas(inner);
    let tone = |n: usize, color| if n > 0 { color } else { DIM };
    f.render_widget(Paragraph::new(Line::from(vec![
        Span::styled(format!(" {} open", open.len()), bold(TEXT)),
        Span::styled("   ● ", fg(tone(overdue, RED))),
        Span::styled(format!("{overdue} overdue"), fg(tone(overdue, RED))),
        Span::styled("   ● ", fg(tone(due, YELLOW))),
        Span::styled(format!("{due} due today"), fg(tone(due, YELLOW))),
        Span::styled("   ✔ ", fg(tone(done, GREEN))),
        Span::styled(format!("{done} done today"), fg(tone(done, GREEN))),
        Span::styled(b.sprints.first().map(|sp| format!("   ◆ {} {}/{} {} · {}d left", sp["name"].as_str().unwrap_or(""),
            sp["done"], sp["scope"], sp["unit"].as_str().unwrap_or(""), sp["days_left"])).unwrap_or_default(), bold(MAUVE)),
        Span::styled(if v.file.is_empty() { String::new() } else { format!("   {}", v.file) }, fg(SUBTLE)),
    ])), left);
    let gauge = match &s.focus {
        Some(fo) => {
            let gone = v.t.saturating_duration_since(fo.started).min(fo.length);
            let left = (fo.length - gone).as_secs();
            let ring = ["○", "◔", "◑", "◕", "●"][(progress(v.t, fo.started, fo.length) * 4.0) as usize];
            Gauge::default().ratio(progress(v.t, fo.started, fo.length)).use_unicode(true)
                .label(format!("{ring} {:02}:{:02} focus {}", left / 60, left % 60, fmt_id(fo.id)))
                .gauge_style(Style::new().fg(c(PEACH)).bg(c(SURFACE)))
        }
        None => Gauge::default().ratio(s.gauge.clamp(0.0, 1.0)).use_unicode(true)
            .label(format!("today {done}/{}", done + doing))
            .gauge_style(Style::new().fg(c(GREEN)).bg(c(SURFACE))),
    };
    f.render_widget(gauge, right);
}

fn columns(f: &mut Frame, area: Rect, s: &State, cols: &[Vec<Task>], v: &View, sel: Option<i64>) {
    let today = start_of_day(v.now);
    let tomorrow = next_day(today);
    let areas = Layout::horizontal([Constraint::Ratio(1, 6); 6]).split(area);
    for (i, col) in cols.iter().enumerate() {
        let (on, color) = (i == s.col, COLUMN[i]);
        let mut block = Block::bordered().border_type(BorderType::Rounded)
            .border_style(if on { bold(color) } else { fg(SUBTLE) })
            .title_top(Line::from(vec![
                Span::styled(" ● ", fg(color)),
                Span::styled(STATUSES[i], if on { bold(color) } else { fg(TEXT) }),
                Span::styled(format!(" {} ", col.len()), fg(DIM)),
            ]));
        let inner = block.inner(areas[i]);
        let fits = (inner.height / 2).max(1) as usize;
        let top = if on { (s.rows[i] as usize + 1).saturating_sub(fits) } else { 0 };
        if col.len() > top + fits {
            block = block.title_bottom(Line::from(Span::styled(format!(" ↓ {} more ", col.len() - top - fits), fg(DIM))).right_aligned());
        }
        f.render_widget(block, areas[i]);
        if col.is_empty() {
            let hint = if on { "press a to add" } else { "·" };
            let row = Rect { y: inner.y + inner.height / 2, height: 1.min(inner.height), ..inner };
            f.render_widget(Paragraph::new(Span::styled(hint, fg(SUBTLE))).centered(), row);
            continue;
        }
        for (r, t) in col.iter().skip(top).take(fits).enumerate() {
            let rect = Rect { y: inner.y + 2 * r as u16, height: 2.min(inner.height.saturating_sub(2 * r as u16)), ..inner };
            f.render_widget(card(t, color, sel == Some(t.id), s.flash, v.t, today, tomorrow, inner.width as usize), rect);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn card(t: &Task, color: Rgb, selected: bool, flash: Option<(i64, Instant)>, now: Instant, today: i64, tomorrow: i64, w: usize) -> Paragraph<'static> {
    let rest = if selected { SURFACE } else { BASE };
    let bg = match flash {
        Some((id, at)) if id == t.id && progress(now, at, FLASH) < 1.0 => Some(mix(mix(color, BASE, 0.35), rest, progress(now, at, FLASH))),
        _ => selected.then_some(SURFACE),
    };
    let bar = Span::styled(if selected { "▌" } else { " " }, fg(color));
    let id = fmt_id(t.id);
    let done = t.status == "done";
    let title_style = if done { fg(DIM).add_modifier(Modifier::CROSSED_OUT) } else if selected { bold(TEXT) } else { fg(TEXT) };
    let first = Line::from(vec![
        bar.clone(),
        Span::styled(if done { "✔ " } else { "● " }, fg(if done { GREEN } else { PRIO[t.priority as usize] })),
        Span::styled(format!("{id} "), fg(DIM)),
        Span::styled(clip(&t.title, w.saturating_sub(4 + id.len())), title_style),
    ]);
    let mut meta = vec![bar, Span::raw("  ")];
    if let Some(d) = t.due_at {
        let (label, tone) = if d < today { (fmt_day(d), RED) } else if d < tomorrow { (format!("today {}", fmt_hm(d)), YELLOW) } else { (fmt_day(d), DIM) };
        meta.push(Span::styled(format!("◷ {label}  "), fg(if done { DIM } else { tone })));
    }
    if let Some(r) = t.blocked_reason.as_ref().filter(|r| !r.is_empty()) {
        meta.push(Span::styled(format!("⊘ {r}  "), fg(RED)));
    }
    if let Some(pts) = t.points {
        meta.push(Span::styled(format!("{pts}p "), bold(MAUVE)));
    }
    if t.subtasks.1 > 0 {
        meta.push(Span::styled(format!("☐{}/{} ", t.subtasks.0, t.subtasks.1), fg(BLUE)));
    }
    if t.spent > 0 {
        meta.push(Span::styled(format!("⧗{} ", fmt_minutes(t.spent)), fg(PEACH)));
    }
    if t.recur.is_some() {
        meta.push(Span::styled("↻ ", fg(GREEN)));
    }
    if let Some(p) = &t.project {
        meta.push(Span::styled(format!("#{p} "), fg(TEAL)));
    }
    for tag in &t.tags {
        meta.push(Span::styled(format!("•{tag} "), fg(PINK)));
    }
    let p = Paragraph::new(vec![first, Line::from(meta)]);
    match bg {
        _ if no_color() && selected => p.style(Style::new().add_modifier(Modifier::REVERSED)),
        Some(bg) => p.style(Style::new().bg(c(bg))),
        None => p,
    }
}

/// Every shown task as one list, most urgent first.
fn list_view(f: &mut Frame, area: Rect, s: &State, cols: &[Vec<Task>], v: &View, sel: Option<i64>) {
    let today = start_of_day(v.now);
    let tomorrow = next_day(today);
    let all = listed(cols);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(bold(BLUE))
        .title_top(Line::from(vec![Span::styled(" ≡ all tasks ", bold(BLUE)), Span::styled(format!("{} ", all.len()), fg(DIM))]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let fits = inner.height.saturating_sub(1) as usize;
    let top = (s.list_row.max(0) as usize + 1).saturating_sub(fits);
    let title_w = (inner.width as usize).saturating_sub(48);
    let mut lines = vec![Line::styled(format!("   {:<9}  {:<6} {:<w$}  {:<12} {}", "status", "id", "title", "project", "due", w = title_w), fg(DIM))];
    for t in all.iter().skip(top).take(fits) {
        let on = sel == Some(t.id);
        let i = STATUSES.iter().position(|x| *x == t.status).unwrap_or(0);
        let due = t.due_at.map(|d| {
            let tone = if t.status == "done" { DIM } else if d < today { RED } else if d < tomorrow { YELLOW } else { DIM };
            Span::styled(fmt_when(d), fg(tone))
        });
        let mut spans = vec![
            Span::styled(if on { "▌" } else { " " }, fg(COLUMN[i])),
            Span::styled("● ", fg(if t.status == "done" { GREEN } else { PRIO[t.priority as usize] })),
            Span::styled(format!("{:<9}  ", t.status), fg(COLUMN[i])),
            Span::styled(format!("{:<6} ", fmt_id(t.id)), fg(DIM)),
            Span::styled(format!("{:<w$}  ", clip(&t.title, title_w), w = title_w), if on { bold(TEXT) } else { fg(TEXT) }),
            Span::styled(format!("{:<12} ", clip(t.project.as_deref().unwrap_or(""), 12)), fg(TEAL)),
        ];
        spans.extend(due);
        let line = Line::from(spans);
        lines.push(if on && !no_color() { line.style(Style::new().bg(c(SURFACE))) } else { line });
    }
    if all.is_empty() {
        lines.push(Line::styled("   nothing matches · esc clears the filters", fg(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn task_panel(f: &mut Frame, area: Rect, t: Option<&Task>, now: i64) {
    let Some(t) = t else {
        let p = Paragraph::new(Span::styled("nothing selected · press a to add a task", fg(DIM)));
        return f.render_widget(p.block(panel(" task ".into(), MAUVE).padding(Padding::horizontal(1))), area);
    };
    let i = STATUSES.iter().position(|s| *s == t.status).unwrap_or(0);
    let p = t.priority as usize;
    let mut lines = vec![
        Line::from(vec![Span::styled(format!("{} ", fmt_id(t.id)), fg(DIM)), Span::styled(t.title.clone(), bold(TEXT))]),
        Line::from(vec![chip(&t.status, COLUMN[i]), Span::raw(" "), chip(PRIORITY[p], PRIO[p])]),
    ];
    let mut meta = vec![];
    if let Some(d) = t.due_at {
        meta.push(Span::styled(format!("◷ due {}   ", fmt_when(d)), fg(if d < start_of_day(now) && t.status != "done" { RED } else { TEXT })));
    }
    if let Some(pr) = &t.project {
        meta.push(Span::styled(format!("#{pr}"), fg(TEAL)));
    }
    if !meta.is_empty() {
        lines.push(Line::from(meta));
    }
    if let Some(r) = t.blocked_reason.as_ref().filter(|r| !r.is_empty()) {
        lines.push(Line::styled(format!("⊘ blocked: {r}"), fg(RED)));
    }
    let mut more = vec![];
    if let Some(sp) = &t.sprint {
        more.push(Span::styled(format!("◆ {sp}   "), fg(MAUVE)));
    }
    if let Some(pts) = t.points {
        more.push(Span::styled(format!("{pts} points   "), fg(MAUVE)));
    }
    if t.subtasks.1 > 0 {
        more.push(Span::styled(format!("☐ {}/{} subtasks   ", t.subtasks.0, t.subtasks.1), fg(BLUE)));
    }
    if let Some(p) = t.parent_id {
        more.push(Span::styled(format!("↳ subtask of {}   ", fmt_id(p)), fg(BLUE)));
    }
    if t.spent > 0 {
        more.push(Span::styled(format!("⧗ {} tracked   ", fmt_minutes(t.spent)), fg(PEACH)));
    }
    if let Some(r) = &t.recur {
        more.push(Span::styled(format!("↻ {r}   "), fg(GREEN)));
    }
    if !t.tags.is_empty() {
        more.push(Span::styled(t.tags.iter().map(|g| format!("•{g}")).collect::<Vec<_>>().join(" "), fg(PINK)));
    }
    if !more.is_empty() {
        lines.push(Line::from(more));
    }
    if !t.description.is_empty() {
        lines.push(Line::styled(t.description.clone(), fg(DIM)));
    }
    let block = panel(format!(" {} ", fmt_id(t.id)), COLUMN[i]).padding(Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(block), area);
}

fn chart_panel(f: &mut Frame, area: Rect, grow: f64, b: &Board, now: i64) {
    let n = b.per_day.len().max(1);
    let total: u64 = b.per_day.iter().sum();
    let max = b.per_day.iter().copied().max().unwrap_or(0).max(1);
    let gap = 1;
    let width = ((area.width.saturating_sub(2) as usize + gap) / n).saturating_sub(gap).max(1) as u16;
    let days = last_days(n, now);
    let bars: Vec<Bar> = b.per_day.iter().zip(&days).enumerate().map(|(i, (&d, &day))| {
        let tone = mix(TEAL, GREEN, i as f64 / (n - 1).max(1) as f64);
        let label: String = fmt_day(day).chars().take(if width >= 2 { 2 } else { 1 }).collect();
        Bar::default()
            .value((d as f64 * grow).round() as u64)
            .text_value(if d > 0 && width >= 2 { d.to_string() } else { String::new() })
            .label(Line::styled(label, fg(if i == n - 1 { TEXT } else { DIM })))
            .style(fg(tone))
            .value_style(Style::new().fg(c(BASE)).bg(c(tone)).add_modifier(Modifier::BOLD))
    }).collect();
    let chart = BarChart::default()
        .block(panel(format!(" done · last {n} days · {total} "), GREEN))
        .data(BarGroup::default().bars(&bars))
        .bar_width(width)
        .bar_gap(gap as u16)
        .max(max);
    f.render_widget(chart, area);
}

fn project_panel(f: &mut Frame, area: Rect, b: &Board) {
    let block = panel(" projects · this week ".into(), TEAL).padding(Padding::horizontal(1));
    if b.projects.is_empty() {
        let lines = vec![Line::styled("no projects yet", fg(DIM)), Line::styled("huddle project add <slug>", fg(SUBTLE))];
        return f.render_widget(Paragraph::new(lines).block(block), area);
    }
    let most = b.projects.iter().map(|p| p.open).max().unwrap_or(0).max(1);
    let name_w = b.projects.iter().map(|p| p.slug.chars().count()).max().unwrap_or(0).min(12);
    let lines: Vec<Line> = b.projects.iter().map(|p| {
        let filled = ((p.open as f64 / most as f64) * 6.0).ceil() as usize;
        Line::from(vec![
            Span::styled(format!("{:<name_w$} ", clip(&p.slug, name_w)), fg(TEAL)),
            Span::styled("▰".repeat(filled), fg(BLUE)),
            Span::styled("▱".repeat(6 - filled), fg(SUBTLE)),
            Span::styled(format!(" {} open", p.open), fg(TEXT)),
            Span::styled(format!(" · {} done", p.done), fg(GREEN)),
        ])
    }).collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn footer(f: &mut Frame, area: Rect, s: &State, t: Instant) {
    let keys = [("?", "help"), ("←→↑↓", "pick"), ("< >", "move"), ("d", "done"), ("b", "block"), ("a", "add"), ("/", "search"), ("tab", "list"), ("f", "focus"), ("u", "undo"), ("s", "stand-up"), ("r", "report"), ("S", "sprint"), ("q", "quit")];
    let hints: Vec<Span> = keys.iter().flat_map(|(k, d)| [Span::styled(format!(" {k}"), bold(PINK)), Span::styled(format!(" {d} "), fg(DIM))]).collect();
    let age = progress(t, s.toast, TOAST);
    let toast = if s.msg.is_empty() || age >= 1.0 { String::new() } else { format!(" {} {} ", if s.err { "✖" } else { "✔" }, s.msg) };
    let width = (toast.chars().count() as u16).min(area.width / 2);
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(width)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(hints)), left);
    if !toast.is_empty() {
        // slides in from the right edge, then fades out over its last second
        let shown = (progress(t, s.toast, Duration::from_millis(200)) * toast.chars().count() as f64).ceil() as usize;
        let tone = mix(if s.err { RED } else { GREEN }, SUBTLE, (age - 0.75) * 4.0);
        let text: String = toast.chars().take(shown).collect();
        f.render_widget(Paragraph::new(Span::styled(clip(&text, width as usize), bold(tone))).right_aligned(), right);
    }
}

fn overlay(f: &mut Frame, area: Rect, title: &str, hint: &str, color: Rgb) -> (Rect, Block<'static>) {
    f.render_widget(Clear, area);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(bold(color))
        .title_top(Span::styled(format!(" {title} "), bold(color)))
        .title_bottom(Line::from(Span::styled(format!(" {hint} "), fg(DIM))).right_aligned())
        .style(Style::new().bg(c(BASE)))
        .padding(Padding::new(2, 2, 1, 0));
    (area, block)
}

fn prompt(f: &mut Frame, area: Rect, p: &Prompt, t: Instant) {
    let ease = 1.0 - (1.0 - progress(t, p.opened, POP)).powi(3); // pops open from 60% width
    let r = centered(area, (64.0 * (0.6 + 0.4 * ease)) as u16, 6);
    let (color, hint) = match p.action {
        _ if p.confirm => (RED, "y delete · any other key keeps it"),
        Action::Search => (PINK, "type to filter · enter keeps it · esc clears"),
        _ => (PINK, "enter save · esc cancel"),
    };
    let (r, block) = overlay(f, r, &p.label, hint, color);
    let lines = if p.confirm {
        vec![Line::styled("It leaves the board; huddle update <id> --restore brings it back.", fg(TEXT))]
    } else {
        let room = r.width.saturating_sub(10) as usize;
        let n = p.value.chars().count();
        let tail: String = p.value.chars().skip(n.saturating_sub(room)).collect();
        let blink = (t.saturating_duration_since(p.opened).as_millis() / 530) % 2 == 0;
        vec![Line::from(vec![Span::styled("❯ ", bold(color)), Span::styled(tail, fg(TEXT)), Span::styled(if blink { "▎" } else { " " }, fg(color))])]
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(block), r);
}

fn help(f: &mut Frame, area: Rect) {
    let keys = [
        ("← → ↑ ↓   h j k l", "pick a task"),
        ("<  >", "move it back or forward"),
        ("d", "mark it done"),
        ("b", "block it, or unblock it"),
        ("a", "add a task to this column"),
        ("e  E", "edit the title, or the description"),
        ("o", "set the project"),
        ("n  g", "set story points, or tags"),
        ("t", "set the due date"),
        ("p", "raise the priority"),
        ("x", "delete it"),
        ("u", "undo the last change"),
        ("f", "focus: 25 minutes on it, or stop"),
        ("/", "search titles and descriptions"),
        ("P", "show one project at a time"),
        ("tab", "switch between columns and a list"),
        ("esc", "clear search and project filter"),
        ("s", "stand-up for today"),
        ("r", "report: this week in numbers"),
        ("S", "sprint: progress and burndown"),
        ("?", "this help"),
        ("q   ctrl-c", "quit"),
    ];
    let lines: Vec<Line> = keys.iter().map(|(k, d)| Line::from(vec![Span::styled(format!("{k:>17}"), bold(PINK)), Span::styled(format!("   {d}"), fg(TEXT))])).collect();
    let (r, block) = overlay(f, centered(area, 56, keys.len() as u16 + 4), "keys", "any key closes", MAUVE);
    f.render_widget(Paragraph::new(lines).block(block), r);
}

fn standup(f: &mut Frame, area: Rect, (y, t): &(Value, Value)) {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let items = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let item = |icon: &str, color: Rgb, text: String| Line::from(vec![Span::styled(format!("  {icon} "), fg(color)), Span::styled(text, fg(TEXT))]);
    let mut lines = vec![Line::from(vec![Span::styled("Yesterday", bold(GREEN)), Span::styled(format!("  {}", s(&y["window"]["label"])), fg(DIM))])];
    let before = lines.len();
    lines.extend(items(&y["done"]).iter().map(|x| item("✔", GREEN, s(&x["title"]))));
    lines.extend(items(&y["moved"]).iter().map(|x| item("→", BLUE, format!("{} ({} → {})", s(&x["title"]), s(&x["from"]), s(&x["to"])))));
    lines.extend(items(&y["commits"]).iter().map(|x| item("◇", MAUVE, format!("{}: {}", s(&x["project"]), s(&x["subject"])))));
    if lines.len() == before {
        lines.push(item("·", DIM, "nothing recorded".into()));
    }

    lines.extend([Line::raw(""), Line::styled("Today", bold(BLUE))]);
    let before = lines.len();
    let mut seen = HashSet::new();
    for (key, icon, color) in [("doing", "▶", YELLOW), ("due_today", "◷", YELLOW), ("overdue", "◷", RED)] {
        for x in items(&t[key]) {
            if seen.insert(s(&x["id"])) {
                let late = if key == "overdue" { " (overdue)" } else { "" };
                lines.push(item(icon, color, format!("{}{late}", s(&x["title"]))));
            }
        }
    }
    if lines.len() == before {
        lines.push(item("·", DIM, "nothing in progress or due".into()));
    }

    lines.extend([Line::raw(""), Line::styled("Blockers", bold(RED))]);
    let blocked = items(&t["blocked"]);
    if blocked.is_empty() {
        lines.push(item("✔", GREEN, "None".into()));
    }
    lines.extend(blocked.iter().map(|x| item("⊘", RED, format!("{}: {}", s(&x["title"]), s(&x["blocked_reason"])))));
    if let Some(r) = t["last_report"]["text"].as_str() {
        lines.extend([Line::raw(""), Line::styled("Last report", bold(MAUVE)), Line::styled(format!("  {r}"), fg(DIM))]);
    }

    let h = (lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let (r, block) = overlay(f, centered(area, 76, h), "stand-up", "any key closes", BLUE);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), r);
}

fn plural(n: i64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn hours_text(h: Option<f64>) -> String {
    match h {
        None => "–".into(),
        Some(h) if h >= 24.0 => format!("{:.1} d", h / 24.0),
        Some(h) => format!("{h:.1} h"),
    }
}

fn tile(f: &mut Frame, area: Rect, label: &str, value: String, sub: String, color: Rgb) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(fg(color));
    let lines = vec![Line::styled(label.to_string(), fg(DIM)), Line::styled(value, bold(color)), Line::styled(sub, fg(DIM))];
    f.render_widget(Paragraph::new(lines).centered().block(block), area);
}

/// The week in numbers: tiles that count up, the charts, what's sitting still and what carries over.
fn report(f: &mut Frame, area: Rect, b: &Board, r: &Value, at: Instant, v: &View) {
    let grow = 1.0 - (1.0 - progress(v.t, at, REPORT)).powi(3);
    let n = |v: &Value| v.as_i64().unwrap_or(0);
    let up = |v: &Value| ((n(v) as f64) * grow).round() as i64;
    let t = &r["totals"];
    let label = format!("report · this week · {}", r["window"]["label"].as_str().unwrap_or(""));
    let (r_area, block) = overlay(f, area, &label, "any key closes", MAUVE);
    let inner = block.inner(r_area);
    f.render_widget(block, r_area);
    let [tiles, mid, low] = Layout::vertical([Constraint::Length(5), Constraint::Fill(1), Constraint::Fill(1)]).areas(inner);

    let cells = Layout::horizontal([Constraint::Ratio(1, 5); 5]).spacing(1).split(tiles);
    let cycle = &r["cycle_time_hours"];
    let blocked = &r["blocked_hours"];
    tile(f, cells[0], "done", up(&t["done"]).to_string(), format!("{} added", n(&t["added"])), GREEN);
    tile(f, cells[1], "streak", format!("{} days", up(&r["streak_days"])), "workdays in a row".into(), PEACH);
    tile(f, cells[2], "cycle time", hours_text(cycle["median"].as_f64()), format!("median of {}", plural(n(&cycle["tasks"]), "task")), BLUE);
    tile(f, cells[3], "blocked", hours_text(blocked["total"].as_f64().filter(|h| *h > 0.0)), plural(n(&blocked["tasks"]), "task"), RED);
    tile(f, cells[4], "open", up(&t["open"]).to_string(), format!("{} doing · {} overdue", n(&t["doing"]), n(&t["overdue"])), YELLOW);

    let [chart, projects] = Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).areas(mid);
    chart_panel(f, chart, grow, b, v.now);
    project_panel(f, projects, b);

    let [still, carry] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(low);
    let status_color = |s: &str| COLUMN[STATUSES.iter().position(|x| *x == s).unwrap_or(0)];
    let still_lines: Vec<Line> = r["sitting_still"].as_array().cloned().unwrap_or_default().iter().map(|x| {
        let days = x["days"].as_f64().unwrap_or(0.0);
        let age = if days >= 5.0 { RED } else if days >= 2.0 { PEACH } else { DIM };
        let status = x["status"].as_str().unwrap_or("");
        Line::from(vec![
            Span::styled(format!("{days:>5.1} d  "), bold(age)),
            Span::styled(format!("{status:<8}"), fg(status_color(status))),
            Span::styled(format!("{} {}", x["id"].as_str().unwrap_or(""), x["title"].as_str().unwrap_or("")), fg(TEXT)),
        ])
    }).collect();
    let carry_lines: Vec<Line> = r["carry_over"].as_array().cloned().unwrap_or_default().iter().map(|x| {
        let status = x["status"].as_str().unwrap_or("");
        let due = x["due"].as_str().map(|d| format!("  ◷ {d}")).unwrap_or_default();
        Line::from(vec![
            Span::styled("▶ ", fg(status_color(status))),
            Span::styled(format!("{} {}", x["id"].as_str().unwrap_or(""), x["title"].as_str().unwrap_or("")), fg(TEXT)),
            Span::styled(due, fg(DIM)),
        ])
    }).collect();
    let empty = |lines: Vec<Line<'static>>, text: &'static str| if lines.is_empty() { vec![Line::styled(text, fg(DIM))] } else { lines };
    f.render_widget(Paragraph::new(empty(still_lines, "nothing in doing, review or blocked")).block(panel(" sitting still ".into(), PEACH).padding(Padding::horizontal(1))), still);
    f.render_widget(Paragraph::new(empty(carry_lines, "nothing carries over")).block(panel(" carries over ".into(), BLUE).padding(Padding::horizontal(1))), carry);
}

/// The running sprint: goal, progress, a burndown that draws itself in, and what's still open.
fn sprint_screen(f: &mut Frame, area: Rect, sp: &Value, at: Instant, t: Instant) {
    let grow = 1.0 - (1.0 - progress(t, at, REPORT)).powi(3);
    let str_ = |v: &Value| v.as_str().unwrap_or("").to_string();
    let n = |v: &Value| v.as_f64().unwrap_or(0.0);
    let title = format!("sprint {} · {} · {} – {}", str_(&sp["id"]), str_(&sp["name"]), str_(&sp["starts"]), str_(&sp["ends"]));
    let (r, block) = overlay(f, area, &title, "any key closes", MAUVE);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let [top, gauge, chart, open] = Layout::vertical([Constraint::Length(2), Constraint::Length(1), Constraint::Fill(2), Constraint::Fill(1)]).spacing(1).areas(inner);

    let goal = str_(&sp["goal"]);
    f.render_widget(Paragraph::new(vec![
        Line::from(vec![Span::styled("goal  ", fg(DIM)), Span::styled(if goal.is_empty() { "—".into() } else { goal }, bold(TEXT))]),
        Line::from(vec![
            Span::styled(format!("{} days left", sp["days_left"]), fg(YELLOW)),
            Span::styled(format!("   {}/{} tasks done", sp["tasks"]["done"], sp["tasks"]["total"]), fg(GREEN)),
            Span::styled(format!("   {}", str_(&sp["state"])), fg(DIM)),
        ]),
    ]), top);
    let (done, scope) = (n(&sp["done"]), n(&sp["scope"]).max(1.0));
    f.render_widget(Gauge::default().ratio((done / scope * grow).clamp(0.0, 1.0)).use_unicode(true)
        .label(format!("{} of {} {}", sp["done"], sp["scope"], str_(&sp["unit"])))
        .gauge_style(Style::new().fg(c(MAUVE)).bg(c(SURFACE))), gauge);

    let days = sp["burndown"].as_array().cloned().unwrap_or_default();
    let shown = ((days.len() as f64) * grow).ceil() as usize;
    let remaining: Vec<(f64, f64)> = days.iter().take(shown).enumerate().map(|(i, d)| (i as f64 + 1.0, n(&d["remaining"]))).collect();
    let ideal: Vec<(f64, f64)> = std::iter::once((0.0, scope)).chain(days.iter().enumerate().map(|(i, d)| (i as f64 + 1.0, n(&d["ideal"])))).collect();
    let remaining: Vec<(f64, f64)> = std::iter::once((0.0, scope)).chain(remaining).collect();
    let span = (days.len() as f64).max(1.0);
    let label = |d: Option<&Value>| Span::styled(d.map(|d| str_(&d["day"])).unwrap_or_default(), fg(DIM));
    let chart_w = Chart::new(vec![
        Dataset::default().name("ideal").marker(Marker::Braille).graph_type(GraphType::Line).style(fg(SUBTLE)).data(&ideal),
        Dataset::default().name("remaining").marker(Marker::Braille).graph_type(GraphType::Line).style(bold(PINK)).data(&remaining),
    ])
    .block(panel(" burndown ".into(), PINK))
    .x_axis(Axis::default().bounds([0.0, span]).style(fg(SUBTLE)).labels([label(days.first()), label(days.last())]))
    .y_axis(Axis::default().bounds([0.0, scope]).style(fg(SUBTLE)).labels([Span::styled("0", fg(DIM)), Span::styled(format!("{scope}"), fg(DIM))]));
    f.render_widget(chart_w, chart);

    let lines: Vec<Line> = sp["open"].as_array().cloned().unwrap_or_default().iter().map(|x| {
        let status = str_(&x["status"]);
        let color = COLUMN[STATUSES.iter().position(|s| *s == status).unwrap_or(0)];
        Line::from(vec![Span::styled(format!("{status:<8} "), fg(color)), Span::styled(format!("{} {}", str_(&x["id"]), str_(&x["title"])), fg(TEXT))])
    }).collect();
    let lines = if lines.is_empty() { vec![Line::styled("everything in this sprint is done", fg(GREEN))] } else { lines };
    f.render_widget(Paragraph::new(lines).block(panel(" still open ".into(), BLUE).padding(Padding::horizontal(1))), open);
}

fn intro(f: &mut Frame, area: Rect, age: Duration, now: i64) {
    let secs = age.as_secs_f64();
    let greet = match hour(now) {
        5..=11 => "Good morning",
        12..=16 => "Good afternoon",
        _ => "Good evening",
    };
    let msg = format!("{greet}! Let's see what's on the board.");
    let typed: String = msg.chars().take((secs / 0.022) as usize).collect();
    let typing = typed.len() < msg.len();
    let eyes = if (0.8..0.95).contains(&secs) { "─ ─" } else { "◠ ◠" }; // one blink
    let filled = ((secs / INTRO.as_secs_f64()) * 24.0).min(24.0) as usize;
    let face = ["╭─────╮".to_string(), format!("│ {eyes} │"), "│  ◡  │".into(), "╰─────╯".into()];
    let gap = Span::raw("   ");
    let lines = vec![
        Line::from([vec![Span::styled(face[0].clone(), fg(PINK)), gap.clone()], gradient("huddle", PINK, BLUE), vec![Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), fg(DIM))]].concat()),
        Line::from(vec![Span::styled(face[1].clone(), fg(PINK)), gap.clone(), Span::styled(typed, fg(TEXT)), Span::styled(if typing { "▎" } else { "" }, fg(PINK))]),
        Line::from([vec![Span::styled(face[2].clone(), fg(PINK)), gap.clone()], gradient(&"▰".repeat(filled), TEAL, MAUVE), vec![Span::styled("▱".repeat(24 - filled), fg(SUBTLE))]].concat()),
        Line::from(vec![Span::styled(face[3].clone(), fg(PINK)), gap, Span::styled(if filled < 24 { "loading your board…" } else { "ready" }, fg(DIM))]),
    ];
    f.render_widget(Paragraph::new(lines), centered(area, 60, 4));
}
