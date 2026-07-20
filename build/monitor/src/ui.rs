use crate::app::{App, TopTab};
use crate::pbs::{Job, JobState};
use ansi_to_tui::IntoText;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

const LEGEND: &str =
    " ←/→ tab · ↑/↓ scroll · ,/. job · a/q/r sections · e/c compact · u user · ^R refresh";
const ERROR_TTL_SECS: u64 = 15;
// Section accent colours; the `┃` column dividers match their section title.
const RUN_COLOR: Color = Color::Blue;
const QUEUE_COLOR: Color = Color::Yellow;

/// Job display id with the `.server` suffix removed: `174170283.gadi-pbs` ->
/// `174170283`, `174190001[3].gadi-pbs` -> `174190001[3]` (the array-task index
/// is kept).
fn strip_server(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

/// True if a job id is an array (sub)job, e.g. `174190001[122].gadi-pbs`.
fn is_array(id: &str) -> bool {
    id.contains('[')
}

/// Grouping key collapsing an array's subjobs: `174190001[122].gadi-pbs` ->
/// `174190001[].gadi-pbs`; non-array ids are returned unchanged.
fn array_group_key(id: &str) -> String {
    match (id.find('['), id.find(']')) {
        (Some(lb), Some(rb)) if rb > lb => format!("{}[]{}", &id[..lb], &id[rb + 1..]),
        _ => id.to_string(),
    }
}

/// Display id for a compacted array: `174190001[122].gadi-pbs` -> `174190001[]`.
fn array_base_display(id: &str) -> String {
    match id.find('[') {
        Some(lb) => format!("{}[]", &id[..lb]),
        None => strip_server(id).to_string(),
    }
}

/// One formatted job row before column alignment.
struct JobRow {
    id: String,
    name: String,
    cpus: String,
    mem: String,
    field5: String, // running: elapsed/req time; waiting: state (Q/H/S)
    field6: String, // running: node; waiting: "req <walltime>"
    suffix: String, // ` ×N` for a compacted array, else empty
    highlighted: bool,
}

/// (text, style) rows for jobs in a class: want_running=true => Running/Exiting;
/// false => Queued|Held|Suspended. When `compact`, array subjobs collapse to a single
/// `<num>[]` row with a ` ×N` task count (like `qstat` without `-t`). Every sub-column
/// (id/name/cpus/mem/…) is padded to the widest value in the section so the columns
/// line up regardless of content length; rows are then packed into display columns by
/// `layout_columns`.
fn job_rows(app: &App, want_running: bool, compact: bool) -> Vec<(String, Style)> {
    let matches: Vec<&Job> = app
        .jobs
        .iter()
        .filter(|job| {
            if want_running {
                job.state.is_running()
            } else {
                job.state.is_waiting()
            }
        })
        .collect();
    let sel = app.selected_job_id.as_deref();

    let row_of = |id: String, job: &Job, suffix: String, highlighted: bool| {
        let cpus = job.cpus.map(|c| format!("{c}c")).unwrap_or_else(|| "--".into());
        let mem = job.mem.clone().unwrap_or_else(|| "--".into());
        let (field5, field6) = if want_running {
            let elapsed = job.elapsed.clone().unwrap_or_else(|| "--".into());
            let req = job.req_walltime.clone().unwrap_or_else(|| "--".into());
            (format!("{:>5}/{:<5}", elapsed, req), display_node(job))
        } else {
            let state = match job.state {
                JobState::Queued => "Q",
                JobState::Held => "H",
                JobState::Suspended => "S",
                _ => "?",
            };
            let req = job.req_walltime.clone().unwrap_or_else(|| "--".into());
            (state.to_string(), format!("req {req}"))
        };
        JobRow { id, name: job.name.clone(), cpus, mem, field5, field6, suffix, highlighted }
    };

    // Resolve to rows (grouping arrays when compact).
    let rows: Vec<JobRow> = if compact {
        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<&Job>> = std::collections::HashMap::new();
        for job in matches {
            let key = array_group_key(&job.id);
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(job);
        }
        order
            .iter()
            .map(|key| {
                let grp = &groups[key];
                let rep = grp[0];
                let hl = grp.iter().any(|j| sel == Some(j.id.as_str()));
                if is_array(&rep.id) {
                    let suffix = if grp.len() > 1 { format!(" \u{d7}{}", grp.len()) } else { String::new() };
                    row_of(array_base_display(&rep.id), rep, suffix, hl)
                } else {
                    row_of(strip_server(&rep.id).to_string(), rep, String::new(), hl)
                }
            })
            .collect()
    } else {
        matches
            .iter()
            .map(|job| row_of(strip_server(&job.id).to_string(), job, String::new(), sel == Some(job.id.as_str())))
            .collect()
    };

    // Align each sub-column to the widest value in this section.
    let iw = rows.iter().map(|r| r.id.chars().count()).max().unwrap_or(0);
    let nw = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(0);
    let cw = rows.iter().map(|r| r.cpus.chars().count()).max().unwrap_or(0);
    let mw = rows.iter().map(|r| r.mem.chars().count()).max().unwrap_or(0);
    let fw = rows.iter().map(|r| r.field5.chars().count()).max().unwrap_or(0);

    rows.iter()
        .map(|r| {
            let raw = format!(
                "{:<iw$} {:<nw$} {:>cw$} {:>mw$}  {:<fw$}  {}{}",
                r.id, r.name, r.cpus, r.mem, r.field5, r.field6, r.suffix,
                iw = iw, nw = nw, cw = cw, mw = mw, fw = fw,
            );
            let style = if r.highlighted && want_running {
                // Selection bar matches the RUNNING JOBS header colour.
                Style::default().fg(RUN_COLOR).add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
            };
            (raw, style)
        })
        .collect()
}

/// Node cell for a running row, with Gadi's noisy `gadi-` prefix trimmed
/// (`gadi-gpu-v100-0100` -> `gpu-v100-0100`).
fn display_node(job: &Job) -> String {
    match &job.node {
        None => "--".into(),
        Some(n) => n.strip_prefix("gadi-").unwrap_or(n).to_string(),
    }
}

/// Heavy full-height vertical bar between columns; box-drawing so stacked rows'
/// separators connect into one continuous line.
const COL_SEP: &str = "\u{2503}"; // ┃
/// Width of the inter-column gap: a full "  ┃  " (2 spaces each side of the divider).
const COL_GAP: usize = 5;

/// Largest column count that fits: `c*col_w + (c-1)*COL_GAP <= width` (at least 1).
fn col_count(col_w: usize, width: usize) -> usize {
    if col_w == 0 {
        1
    } else {
        ((width + COL_GAP) / (col_w + COL_GAP)).max(1)
    }
}

/// Widest row in a section (display columns).
fn rows_width(rows: &[(String, Style)]) -> usize {
    rows.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0)
}

/// Total section height (title + content lines) needed to show every row, given the
/// column count that fits `width`. 0 for an empty section.
fn section_need(rows: &[(String, Style)], width: u16) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let c = col_count(rows_width(rows), width as usize);
    1 + (rows.len() + c - 1) / c
}

/// Split `avail` rows of vertical space between two sections needing `r` and `q`.
/// If both fit, give each its need; otherwise split equally, letting a section that
/// needs less than half donate the remainder to the other.
fn split_heights(r: usize, q: usize, avail: usize) -> (usize, usize) {
    if r + q <= avail {
        return (r, q);
    }
    let half = avail / 2;
    let mut rh = r.min(half);
    let mut qh = q.min(half);
    let mut rem = avail - rh - qh;
    if r > rh {
        let g = rem.min(r - rh);
        rh += g;
        rem -= g;
    }
    if q > qh {
        qh += rem.min(q - qh);
    }
    (rh, qh)
}

/// Content lines for a section capped to `max_h` total rows (title included). If the
/// rows don't fit, one line becomes a dim "N hidden" note and only a page of rows is
/// shown --- the page containing `selected` (its flat row index), so sweeping the
/// selection keeps the highlighted row on screen. `selected` = None shows the first page.
fn section_lines(
    rows: Vec<(String, Style)>,
    max_h: usize,
    width: u16,
    sep_color: Color,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    let content = max_h.saturating_sub(1); // minus the title/border row
    if rows.is_empty() || content == 0 {
        return Vec::new();
    }
    let c = col_count(rows_width(&rows), width as usize);
    let n = rows.len();
    if n <= content * c {
        return layout_columns(&rows, width, sep_color, None);
    }
    // Truncated: show one page of `cap` rows (reserving a line for the note), scrolled
    // to the page holding `selected`.
    let cap = content.saturating_sub(1) * c;
    let (start, end) = if cap == 0 {
        (0, 0)
    } else {
        let s = selected.map(|s| (s.min(n - 1) / cap) * cap).unwrap_or(0);
        (s, (s + cap).min(n))
    };
    let hidden = n - (end - start);
    let mut lines = if end > start {
        layout_columns(&rows[start..end], width, sep_color, None)
    } else {
        Vec::new()
    };
    lines.push(Line::from(Span::styled(
        format!("\u{2026} {hidden} hidden"),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    )));
    lines
}

/// Lay cells into as many side-by-side columns as `width` allows, filling
/// column-major (left column first). Columns are `col_w` wide (the widest cell) and
/// separated by a `┃` divider (coloured `sep_color`, matching the section title) in a
/// gap of at most 5 columns ("  ┃  "); surplus width is left as trailing space rather
/// than stretching the columns apart. Each cell is padded to `col_w` so selected-row
/// highlighting and alignment stay clean. If `header` is given it is repeated above
/// every column. Single column when only one fits.
fn layout_columns(
    rows: &[(String, Style)],
    width: u16,
    sep_color: Color,
    header: Option<(String, Style)>,
) -> Vec<Line<'static>> {
    let n = rows.len();
    if n == 0 {
        return Vec::new();
    }
    let hdr_w = header.as_ref().map(|(s, _)| s.chars().count()).unwrap_or(0);
    let col_w = rows_width(rows).max(hdr_w);
    let width = width as usize;

    let cols = col_count(col_w, width).min(n);
    if cols <= 1 {
        let mut lines = Vec::with_capacity(n + 1);
        if let Some((h, hs)) = &header {
            lines.push(Line::from(Span::styled(format!("{:<width$}", h, width = col_w), *hs)));
        }
        lines.extend(rows.iter().map(|(s, st)| Line::from(Span::styled(s.clone(), *st))));
        return lines;
    }

    let k = (n + cols - 1) / cols; // rows per column (display height)
    let cols = (n + k - 1) / k; // trim any empty trailing columns
    // Uniform gap capped at COL_GAP ("  ┃  "); surplus width stays as trailing space.
    let gap_w = ((width - cols * col_w) / (cols - 1)).min(COL_GAP);
    let left = (gap_w - 1) / 2; // divider centred in the gap
    let sep_style = Style::default().fg(sep_color);
    let divider = |spans: &mut Vec<Span<'static>>| {
        spans.push(Span::raw(" ".repeat(left)));
        spans.push(Span::styled(COL_SEP, sep_style));
        spans.push(Span::raw(" ".repeat(gap_w - 1 - left)));
    };

    let mut lines = Vec::with_capacity(k + 1);
    if let Some((h, hs)) = &header {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(cols * 2);
        for j in 0..cols {
            if j > 0 {
                divider(&mut spans);
            }
            spans.push(Span::styled(format!("{:<width$}", h, width = col_w), *hs));
        }
        lines.push(Line::from(spans));
    }
    for i in 0..k {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(cols * 2);
        for j in 0..cols {
            let cell = rows.get(j * k + i); // column-major: column j is rows[j*k..]
            if j > 0 {
                // Always draw the divider (even above an empty trailing cell) so the
                // ┃ bar is continuous from the top row to the bottom.
                divider(&mut spans);
            }
            match cell {
                Some((raw, style)) => {
                    spans.push(Span::styled(format!("{:<width$}", raw, width = col_w), *style))
                }
                None => spans.push(Span::raw(" ".repeat(col_w))),
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The Job Usage tab content: the ANSI panel rendered by `usage::render`
/// (summary + one row per running job). Used both to render the tab and to
/// size the top pane.
fn usage_text(app: &App) -> Text<'static> {
    app.usage
        .as_str()
        .into_text()
        .unwrap_or_else(|_| Text::raw(app.usage.clone()))
}

fn freshness_line(app: &App) -> Line<'static> {
    let usage = app
        .usage_at
        .map(|t| format!("usage {}s", t.elapsed().as_secs()))
        .unwrap_or_else(|| "usage --".into());
    let jobs = app
        .jobs_at
        .map(|t| format!("jobs {}s", t.elapsed().as_secs()))
        .unwrap_or_else(|| "jobs --".into());
    let who = if app.project.is_empty() {
        app.user.clone()
    } else {
        format!("{} · {}", app.user, app.project)
    };
    let mut s = format!("{who} · {usage} · {jobs} ");
    if let Some((msg, at)) = &app.last_error {
        if at.elapsed().as_secs() <= ERROR_TTL_SECS {
            s = format!("!{msg} · {s}");
        }
    }
    Line::from(Span::styled(s, Style::default().fg(Color::DarkGray)))
}

fn tab_title(app: &App) -> Line<'static> {
    // Each tab keeps its own fixed colour; only the active tab gains the
    // reverse-video/bold highlight.
    let tab = |label: &'static str, color: Color, active: bool| {
        let mut style = Style::default().fg(color);
        if active {
            style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        Span::styled(label, style)
    };
    Line::from(vec![
        tab(" Job Usage ", Color::Red, app.top_tab == TopTab::Usage),
        Span::raw(" "),
        tab(
            " Log Preview ",
            Color::Magenta,
            app.top_tab == TopTab::LogPreview,
        ),
        Span::raw(" "),
        tab(" Details ", Color::Gray, app.top_tab == TopTab::Details),
    ])
}

/// `u_text` is the Job Usage tab's content, prebuilt once by `draw` (it is also
/// needed there to size this pane) and handed in so it isn't rebuilt.
fn render_top(f: &mut Frame, area: Rect, app: &mut App, u_text: Text<'static>) {
    let block = Block::default().borders(Borders::TOP).title(tab_title(app));
    let inner_h = area.height.saturating_sub(1) as usize; // minus the TOP border row
    let tab = app.top_tab;
    app.top_inner_h = inner_h;

    // Total line count per tab, without materialising the whole content.
    let total = match tab {
        TopTab::Usage => u_text.lines.len(),
        TopTab::LogPreview => app.log.len(),
        TopTab::Details => app.details.lines().count(),
    };
    let max_off = total.saturating_sub(inner_h);

    // Resolve the scroll offset (disjoint mutable borrow of the active tab's fields).
    let (follow, scroll) = app.active();
    let off = if *follow {
        *scroll = max_off; // pin to the bottom
        max_off
    } else {
        let clamped = (*scroll).min(max_off);
        *scroll = clamped; // write back the clamp (prevents unbounded offsets)
        if clamped >= max_off {
            *follow = true; // scrolled back to the bottom -> re-engage follow
        }
        clamped
    };

    // Build the Text to render. The log can be huge (100k+ lines), so slice to the
    // visible window rather than cloning every line each frame; scroll is then 0.
    let (text, scroll_off): (Text, u16) = match tab {
        TopTab::Usage => (u_text, off.min(u16::MAX as usize) as u16),
        TopTab::Details => (Text::raw(app.details.clone()), off.min(u16::MAX as usize) as u16),
        TopTab::LogPreview => {
            // Lines are sanitized at ingestion (logs::sanitize_log_line), so plain here.
            let end = (off + inner_h).min(app.log.len());
            let lines: Vec<Line> = app.log[off..end].iter().map(|l| Line::from(l.clone())).collect();
            (Text::from(lines), 0)
        }
    };
    f.render_widget(Paragraph::new(text).scroll((scroll_off, 0)).block(block), area);
}

fn render_section(f: &mut Frame, area: Rect, title: &str, color: Color, content: Text<'static>) {
    let block = Block::default().borders(Borders::TOP).title(Span::styled(
        format!(" {title} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(content).block(block), area);
}

#[derive(Clone, Copy)]
enum Slot {
    Top,
    Gap,
    Running,
    Queued,
    Array,
    Status,
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let width = area.width;

    let array_text: Option<Text> = app
        .array
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|raw| raw.into_text().unwrap_or_else(|_| Text::raw(raw.to_string())));

    // Expanded rows decide visibility and whether we must auto-compact.
    let run_exp = job_rows(app, true, false);
    let que_exp = job_rows(app, false, false);
    let show_running = !run_exp.is_empty() && app.show_running;
    let show_queued = !que_exp.is_empty() && app.show_queued;
    let show_array = array_text.is_some() && app.show_array;

    // The top pane is kept at least tall enough to show the full Job Usage content;
    // the job sections share whatever is left (below the status line and array box).
    // Built once: used here to size the pane and handed to render_top to draw.
    // +1 for the pane's top border/title row (its content area is height - 1).
    let u_text = usage_text(app);
    let u_height = u_text.lines.len() + 1;
    let top_min = u_height.max(3).min((area.height as usize).saturating_sub(1).max(1));
    let gaps = show_running as usize + show_queued as usize + show_array as usize;
    let array_h = array_text.as_ref().map(|t| 1 + t.lines.len()).unwrap_or(0);
    // Reserve 2 rows at the bottom: a blank separator + the status bar.
    let avail = (area.height as usize).saturating_sub(top_min + 2 + gaps + array_h);

    // Auto-compact when the expanded lists can't fit; a manual e/c toggle overrides.
    let need_r_exp = if show_running { section_need(&run_exp, width) } else { 0 };
    let need_q_exp = if show_queued { section_need(&que_exp, width) } else { 0 };
    let auto_compact = need_r_exp + need_q_exp > avail;
    let compact = app.compact_override.unwrap_or(auto_compact);
    app.compact = compact; // cache the effective state for the e/c toggle

    let run_rows = if compact { job_rows(app, true, true) } else { run_exp };
    let que_rows = if compact { job_rows(app, false, true) } else { que_exp };

    // Split the budget, then cap each section (adding a "N hidden…" note if needed).
    let need_r = if show_running { section_need(&run_rows, width) } else { 0 };
    let need_q = if show_queued { section_need(&que_rows, width) } else { 0 };
    let (r_h, q_h) = split_heights(need_r, need_q, avail);
    // Expanded rows map 1:1 to the running-job selection, so page to it; compact rows
    // don't (arrays are grouped), so show the first page there.
    let run_sel = if compact { None } else { Some(app.selected) };
    let running = section_lines(run_rows, r_h, width, RUN_COLOR, run_sel);
    let queued = section_lines(que_rows, q_h, width, QUEUE_COLOR, None);

    // Top keeps its Job-Usage minimum; a section that got squeezed to nothing is
    // dropped entirely (rather than showing a lone title) so it can't steal that space.
    let mut slots: Vec<(Slot, Constraint)> = vec![(Slot::Top, Constraint::Min(top_min as u16))];
    if show_running && !running.is_empty() {
        slots.push((Slot::Gap, Constraint::Length(1)));
        slots.push((Slot::Running, Constraint::Length(1 + running.len() as u16)));
    }
    if show_queued && !queued.is_empty() {
        slots.push((Slot::Gap, Constraint::Length(1)));
        slots.push((Slot::Queued, Constraint::Length(1 + queued.len() as u16)));
    }
    if show_array {
        let n = array_text.as_ref().unwrap().lines.len() as u16;
        slots.push((Slot::Gap, Constraint::Length(1)));
        slots.push((Slot::Array, Constraint::Length(1 + n)));
    }
    // Blank line separating the last section (or the top pane) from the toolbar.
    slots.push((Slot::Gap, Constraint::Length(1)));
    slots.push((Slot::Status, Constraint::Length(1)));

    let constraints: Vec<Constraint> = slots.iter().map(|(_, c)| *c).collect();
    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Each of these is built once above and consumed by its single slot below, so wrap
    // in Option and `take` rather than cloning per render.
    let mut u_text = Some(u_text);
    let mut running = Some(running);
    let mut queued = Some(queued);
    let mut array_text = array_text;

    for ((slot, _), rect) in slots.iter().zip(rects.iter()) {
        match slot {
            Slot::Top => render_top(f, *rect, app, u_text.take().unwrap_or_default()),
            Slot::Gap => {}
            Slot::Running => render_section(
                f,
                *rect,
                "RUNNING JOBS",
                RUN_COLOR,
                Text::from(running.take().unwrap_or_default()),
            ),
            Slot::Queued => render_section(
                f,
                *rect,
                "QUEUED & HELD JOBS",
                QUEUE_COLOR,
                Text::from(queued.take().unwrap_or_default()),
            ),
            Slot::Array => render_section(
                f,
                *rect,
                "ARRAY JOB PROGRESS",
                Color::Green,
                array_text.take().unwrap_or_default(),
            ),
            Slot::Status => {
                if let Some(buf) = &app.input {
                    // Username box open: the whole status line becomes an edit prompt,
                    // with the best fuzzy match shown as a suggestion (Enter adopts it).
                    let suggestion = app.user_suggestion();
                    let mut spans = vec![
                        Span::styled(
                            " Switch user: ",
                            Style::default().fg(Color::Black).bg(Color::Cyan),
                        ),
                        Span::styled(
                            format!("{buf}\u{2588}"), // block cursor
                            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                    ];
                    if let Some(s) = &suggestion {
                        spans.push(Span::styled(
                            format!(" \u{2192} {s}"),
                            Style::default().fg(Color::Green),
                        ));
                    }
                    spans.push(Span::styled(
                        "  (Enter=switch, Esc=cancel)",
                        Style::default().fg(Color::DarkGray),
                    ));
                    f.render_widget(Paragraph::new(Line::from(spans)), *rect);
                    continue;
                }
                // Legend takes priority (always shown in full); the freshness fills the
                // remainder, right-aligned (clipped only if the terminal is too narrow).
                let legend = Line::from(Span::styled(LEGEND, Style::default().fg(Color::Cyan)));
                let legend_w = legend.width() as u16;
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(legend_w), Constraint::Min(0)])
                    .split(*rect);
                f.render_widget(Paragraph::new(legend), chunks[0]);
                f.render_widget(
                    Paragraph::new(freshness_line(app)).alignment(Alignment::Right),
                    chunks[1],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Update;
    use crate::pbs::Job;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn job(id: &str, state: JobState, node: Option<&str>) -> Job {
        Job {
            id: id.into(),
            owner: "bh5941".into(),
            queue: "normal-exec".into(),
            name: "code".into(),
            state,
            node: node.map(|s| s.into()),
            cpus: None,
            mem: None,
            req_walltime: None,
            elapsed: None,
        }
    }

    #[test]
    fn layout_columns_packs_columns_with_capped_gap() {
        let rows: Vec<(String, Style)> = (0..6)
            .map(|i| (format!("job{i}"), Style::default())) // col_w = 4
            .collect();
        // Too narrow: one row per job.
        assert_eq!(layout_columns(&rows, 6, Color::Green, None).len(), 6);
        // Very wide: all six pack onto one line with five ┃ dividers, but the gap is
        // capped at 5 ("  ┃  ") and columns left-pack instead of stretching to width.
        let wide = layout_columns(&rows, 100, Color::Green, None);
        assert_eq!(wide.len(), 1);
        let text: String = wide[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.matches('\u{2503}').count(), 5); // one divider between each pair
        assert_eq!(text.chars().count(), 6 * 4 + 5 * 5); // 6 cells + 5 gaps of 5 = 49
        assert!(!text.contains("   ")); // no gap wider than "  ┃  " (max 2 spaces)
        // Intermediate width fitting exactly two columns -> three display rows.
        assert_eq!(layout_columns(&rows, 14, Color::Green, None).len(), 3);
        // Single row never splits.
        assert_eq!(layout_columns(&rows[..1], 200, Color::Green, None).len(), 1);
    }

    #[test]
    fn layout_columns_divider_spans_every_row() {
        // 3 rows over 2 columns -> 2 display rows; the bottom row's right cell is empty,
        // but the divider is still drawn so the ┃ bar reaches the bottom.
        let rows: Vec<(String, Style)> = (0..3)
            .map(|i| (format!("job{i}"), Style::default()))
            .collect();
        let lines = layout_columns(&rows, 20, Color::Green, None);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(text.contains('\u{2503}')); // every row carries the divider
        }
    }

    #[test]
    fn compact_collapses_array_subjobs() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![
            job("174190001[1].gadi-pbs", JobState::Running, None),
            job("174190001[2].gadi-pbs", JobState::Running, None),
            job("174190001[3].gadi-pbs", JobState::Running, None),
            job("174200000.gadi-pbs", JobState::Running, None),
        ]));
        assert_eq!(job_rows(&app, true, false).len(), 4); // expanded: one row per subjob
        let c = job_rows(&app, true, true);
        assert_eq!(c.len(), 2); // array collapses to one row, plus the plain job
        assert!(c[0].0.contains("174190001[]")); // base form
        assert!(c[0].0.contains("\u{d7}3")); // ×3 task count
        assert!(!c[0].0.contains("174190001[1]")); // subjob index gone
        assert!(c.iter().any(|(s, _)| s.contains("174200000")));
    }

    #[test]
    fn suspended_jobs_show_in_queued_section() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![
            job("1.gadi-pbs", JobState::Suspended, None),
            job("2.gadi-pbs", JobState::Queued, None),
        ]));
        assert!(job_rows(&app, true, false).is_empty()); // neither is running
        let rows = job_rows(&app, false, false);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0.contains(" S ") || rows[0].0.contains("S  ")); // state letter S
    }

    #[test]
    fn node_prefix_trimmed_in_running_rows() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![job(
            "1.gadi-pbs",
            JobState::Running,
            Some("gadi-gpu-v100-0100"),
        )]));
        let rows = job_rows(&app, true, false);
        assert!(rows[0].0.contains("gpu-v100-0100"));
        assert!(!rows[0].0.contains("gadi-gpu"));
    }

    #[test]
    fn split_heights_equalizes_when_over() {
        assert_eq!(split_heights(3, 4, 20), (3, 4)); // fits: natural sizes
        assert_eq!(split_heights(50, 50, 20), (10, 10)); // both large: equal halves
        assert_eq!(split_heights(3, 50, 20), (3, 17)); // small donates to large
    }

    #[test]
    fn section_lines_caps_with_hidden_note() {
        let rows: Vec<(String, Style)> = (0..10)
            .map(|i| (format!("job{i}"), Style::default())) // col_w 4, width 8 -> 1 column
            .collect();
        let lines = section_lines(rows, 4, 8, Color::Green, None); // height 4 => 3 content lines
        assert_eq!(lines.len(), 3); // 2 rows + the note
        let note: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(note.contains("hidden"));
        assert!(note.contains('8')); // 10 total - 2 shown = 8 hidden
    }

    #[test]
    fn section_lines_pages_to_selected() {
        let rows: Vec<(String, Style)> = (0..10)
            .map(|i| (format!("job{i}"), Style::default())) // width 8 -> 1 col, cap 2/page
            .collect();
        // Selecting job 5 -> page 2 (rows 4,5), so job5 stays visible, job0 scrolls off.
        let lines = section_lines(rows, 4, 8, Color::Green, Some(5));
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("job4") && text.contains("job5"));
        assert!(!text.contains("job0"));
        assert!(text.contains("hidden"));
    }

    #[test]
    fn draw_auto_compacts_when_overflowing() {
        let mut app = App::new();
        let jobs: Vec<Job> = (1..=40)
            .map(|i| job(&format!("77[{i}].gadi-pbs"), JobState::Running, None))
            .collect();
        app.apply(Update::Jobs(jobs));
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let dump: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(app.compact); // auto-compaction kicked in
        assert!(dump.contains("77[]")); // collapsed array
        assert!(dump.contains("\u{d7}40")); // ×40 tasks
    }

    #[test]
    fn strip_server_keeps_number_and_array_task() {
        assert_eq!(strip_server("174170283.gadi-pbs"), "174170283");
        assert_eq!(strip_server("174190001[3].gadi-pbs"), "174190001[3]");
        assert_eq!(strip_server("42"), "42"); // no suffix: unchanged
    }

    #[test]
    fn renders_tabs_sections_and_status() {
        let mut app = App::new();
        app.user = "bh5941".into();
        app.project = "xr78".into();
        app.apply(Update::Usage("1 running (12 cores)".into()));
        app.apply(Update::Jobs(vec![
            Job {
                id: "174170283.gadi-pbs".into(),
                owner: "bh5941".into(),
                queue: "gpuvolta-exec".into(),
                name: "train".into(),
                state: JobState::Running,
                node: Some("gadi-gpu-v100-0100".into()),
                cpus: Some(12),
                mem: Some("90gb".into()),
                req_walltime: Some("01:00".into()),
                elapsed: Some("00:05".into()),
            },
            job("174187629.gadi-pbs", JobState::Queued, None),
        ]));
        app.apply(Update::Log {
            lines: vec!["log line one".into()],
            replace: true,
        });

        // Wide enough for the 84-column legend plus the right-aligned freshness
        // text (which carries the user · project prefix).
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let dump: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(dump.contains("Job Usage"));
        assert!(dump.contains("Log Preview"));
        assert!(dump.contains("RUNNING JOBS"));
        assert!(dump.contains("QUEUED & HELD JOBS"));
        // Job rows show the number with the `.gadi-pbs` server suffix stripped.
        assert!(dump.contains("174170283 ")); // running row, stripped id
        assert!(dump.contains("174187629 ")); // queued row, stripped id
        assert!(!dump.contains("174187629.gadi-pbs")); // suffix gone from the rows
        assert!(dump.contains("scroll")); // status-bar legend
        assert!(dump.contains("1 running (12 cores)")); // usage tab content
        assert!(dump.contains("12c")); // ncpus column
        assert!(dump.contains("00:05/01:00")); // elapsed/requested walltime
        assert!(dump.contains("xr78")); // project in the freshness line

        // Job Usage is the default tab, so the log is not shown until we switch.
        assert!(!dump.contains("log line one"));
        app.next_tab();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let dump2: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(dump2.contains("log line one"));
    }
}
