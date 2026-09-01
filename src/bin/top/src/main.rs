use std::{
    collections::BTreeMap,
    io::{Write, stdout},
    time::{Duration, Instant},
};

use crossterm::{
    ExecutableCommand, QueueableCommand,
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode, size,
    },
};
use monitor_api::CompartmentHandle;
use twizzler::object::{Object, TypedObject};
use twizzler_abi::{
    object::ObjID,
    syscall::{
        EnumerateKind, ThreadSchedStats, ThreadSctxIds, sys_info, sys_object_enumerate_notes,
        sys_object_get_note, sys_thread_read_sctx_ids, sys_thread_read_stats, sys_thread_self_id,
        sys_thread_stats,
    },
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

const REFRESH: Duration = Duration::from_millis(1000);

/// Which thread rows to draw. Compartment lines are always drawn -- hiding threads is what
/// turns this display into a per-compartment summary, so the summary has to survive it.
#[derive(Copy, Clone, PartialEq, Eq, Default)]
enum ThreadView {
    #[default]
    All,
    /// Hide threads belonging to no compartment: kernel threads, and the userspace ones with a
    /// zero home (the monitor's own, bootstrap, statically-linked programs). Defined by the
    /// repr flag and the home context rather than by a list of server names, so nothing has to
    /// be kept in sync as compartments come and go.
    HideSystem,
    HideAll,
}

impl ThreadView {
    fn next(&self) -> Self {
        match self {
            ThreadView::All => ThreadView::HideSystem,
            ThreadView::HideSystem => ThreadView::HideAll,
            ThreadView::HideAll => ThreadView::All,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ThreadView::All => "all threads",
            ThreadView::HideSystem => "hiding system threads",
            ThreadView::HideAll => "compartments only",
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let batch = args.iter().any(|a| a == "-b" || a == "--batch");

    let mut tracker = ThreadTracker::default();
    // Sets the starting mode; the interactive display can still cycle from here with `t`.
    if args.iter().any(|a| a == "--compartments" || a == "-c") {
        tracker.view = ThreadView::HideAll;
    } else if args.iter().any(|a| a == "--hide-system" || a == "-s") {
        tracker.view = ThreadView::HideSystem;
    }

    if batch {
        // Percentages are per-interval, so one sample has nothing to compare against.
        tracker.sample();
        std::thread::sleep(REFRESH);
        tracker.sample();
        tracker.render_plain(&mut stdout()).unwrap();
        return;
    }

    enable_raw_mode().unwrap();
    let mut out = stdout();
    out.execute(EnterAlternateScreen).unwrap();
    out.execute(Hide).unwrap();

    tracker.sample();
    tracker.render(&mut out).unwrap();

    while !wait_for_input(REFRESH, &mut tracker, &mut out) {
        tracker.sample();
        tracker.render(&mut out).unwrap();
    }

    let _ = out.execute(Show);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Waits out the refresh interval, returning true if the user asked to quit. A mode change is
/// redrawn immediately rather than at the next sample, so the display tracks the keypress.
fn wait_for_input(timeout: Duration, tracker: &mut ThreadTracker, out: &mut impl Write) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        if event::poll(deadline - now).unwrap() {
            if let Event::Key(key) = event::read().unwrap() {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return true,
                    KeyCode::Char('t') => {
                        tracker.view = tracker.view.next();
                        tracker.render(out).unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
}

struct ThreadInfo {
    id: ObjID,
    name: Option<String>,
    state: Option<ExecutionState>,
    /// Mapped once and kept. Remapping every sample churns the compartment's slots and the
    /// monitor's handle cache for a value that is one atomic load away.
    repr: Option<Object<ThreadRepr>>,
    /// Cumulative statticks, as of the last sample.
    stats: ThreadSchedStats,
    /// Statticks charged during the last sample interval.
    delta: ThreadSchedStats,
    /// True until this thread has been sampled twice; its first delta would be its whole
    /// lifetime, not an interval.
    fresh: bool,
    err: Option<TwzError>,
    seen: bool,
    /// Home and active security contexts, or None if the read failed. A thread whose active
    /// context differs from its home is inside a cross-compartment call.
    sctx: Option<ThreadSctxIds>,
    /// From the repr's flags. The security context ids cannot supply this: the kernel context
    /// and the monitor instance are both id zero.
    kernel: bool,
}

impl ThreadInfo {
    fn cpu_ticks(&self) -> u64 {
        self.stats.user.saturating_add(self.stats.system)
    }

    fn delta_cpu_ticks(&self) -> u64 {
        self.delta.user.saturating_add(self.delta.system)
    }

    /// Fraction of one cpu used over the last interval. `elapsed` is the interval in
    /// statticks; a thread cannot have run for more of it than that.
    fn cpu(&self, elapsed: u64) -> f64 {
        frac(self.delta_cpu_ticks(), elapsed)
    }

    fn user(&self, elapsed: u64) -> f64 {
        frac(self.delta.user, elapsed)
    }

    fn system(&self, elapsed: u64) -> f64 {
        frac(self.delta.system, elapsed)
    }
}

impl ThreadInfo {
    /// The compartment this thread belongs to. Zero covers everything with no compartment:
    /// kernel threads, statically-linked ones, and the monitor's own.
    fn home(&self) -> ObjID {
        self.sctx.map(|s| s.home).unwrap_or(ObjID::new(0))
    }

    fn is_cross(&self) -> bool {
        self.sctx.is_some_and(|s| s.is_cross())
    }
}

fn frac(ticks: u64, elapsed: u64) -> f64 {
    if elapsed == 0 {
        0.0
    } else {
        (ticks as f64 / elapsed as f64).min(1.0)
    }
}

fn total(stats: &ThreadSchedStats) -> u64 {
    stats
        .user
        .saturating_add(stats.system)
        .saturating_add(stats.idle)
}

/// One compartment's worth of threads, as displayed.
struct CompGroup<'a> {
    label: String,
    threads: Vec<&'a ThreadInfo>,
    /// Statticks charged to the group over the last interval, for ordering groups.
    ticks: u64,
    /// Threads currently executing in some other compartment.
    cross: usize,
    /// The group belongs to no compartment -- see [ThreadView::HideSystem].
    system: bool,
}

impl CompGroup<'_> {
    /// Fraction of one cpu the whole group used over the last interval. Summed from the same
    /// per-thread figures the rows show, so a collapsed group still reports what it cost.
    fn cpu(&self, elapsed: u64) -> f64 {
        self.threads.iter().map(|t| t.cpu(elapsed)).sum()
    }
}

#[derive(Default)]
struct ThreadTracker {
    threads: BTreeMap<ObjID, ThreadInfo>,
    /// Statticks in the last interval, taken as the largest per-thread total delta. Every
    /// thread alive across the whole interval advances by exactly the elapsed time (the
    /// kernel charges the time a thread wasn't scheduled to its idle counter), so the max
    /// is that time, and a thread created part way through can only report less.
    elapsed: u64,
    ticks_per_sec: f64,
    last_sample: Option<Instant>,
    /// Compartment instance id -> name, `None` for one the monitor would not name for us.
    /// Cached across samples: each miss costs a gate call into the monitor, and both hits and
    /// misses are stable for the life of a compartment.
    comps: BTreeMap<ObjID, Option<String>>,
    view: ThreadView,
}

impl ThreadTracker {
    fn sample(&mut self) {
        let now = Instant::now();
        self.scan_for_threads();
        self.read_thread_stats();
        self.read_thread_names();
        self.read_thread_sctx();
        self.resolve_comp_names();

        self.elapsed = self
            .threads
            .values()
            .map(|t| total(&t.delta))
            .max()
            .unwrap_or(0);
        if let Some(last) = self.last_sample {
            let secs = now.duration_since(last).as_secs_f64();
            if secs > 0.0 && self.elapsed > 0 {
                self.ticks_per_sec = self.elapsed as f64 / secs;
            }
        }
        self.last_sample = Some(now);
    }

    fn scan_for_threads(&mut self) {
        for thread in self.threads.values_mut() {
            thread.seen = false;
        }

        let mut buf = [ObjID::default(); 128];
        let mut offset = 0;

        loop {
            match twizzler_abi::syscall::sys_enumerate(EnumerateKind::Threads, &mut buf, offset) {
                Ok(count) => {
                    if count == 0 {
                        break;
                    }

                    for id in &buf[0..count] {
                        self.threads
                            .entry(*id)
                            .and_modify(|t| t.seen = true)
                            .or_insert_with(|| ThreadInfo {
                                id: *id,
                                name: None,
                                state: None,
                                repr: None,
                                stats: ThreadSchedStats::default(),
                                delta: ThreadSchedStats::default(),
                                fresh: true,
                                err: None,
                                seen: true,
                                sctx: None,
                                kernel: false,
                            });
                    }

                    offset += count;
                }
                Err(_) => break,
            }
        }

        // A thread is only dropped when it leaves the kernel's thread list. A failed stat or
        // map is transient (or a permission problem) and must not silently empty the table.
        self.threads.retain(|_, t| t.seen);
    }

    fn read_thread_stats(&mut self) {
        for thread in self.threads.values_mut() {
            let mut stats = ThreadSchedStats::default();
            match sys_thread_read_stats(thread.id, &mut stats) {
                Ok(()) => {
                    thread.delta = if thread.fresh {
                        ThreadSchedStats::default()
                    } else {
                        ThreadSchedStats {
                            user: stats.user.saturating_sub(thread.stats.user),
                            system: stats.system.saturating_sub(thread.stats.system),
                            idle: stats.idle.saturating_sub(thread.stats.idle),
                        }
                    };
                    thread.stats = stats;
                    thread.fresh = false;
                    thread.err = None;
                }
                Err(e) => {
                    thread.delta = ThreadSchedStats::default();
                    thread.err = Some(e);
                }
            }

            if thread.repr.is_none() {
                thread.repr = Object::<ThreadRepr>::map(thread.id, MapFlags::READ).ok();
            }
            thread.state = thread.repr.as_ref().map(|repr| repr.base().get_state());
            thread.kernel = thread
                .repr
                .as_ref()
                .is_some_and(|repr| repr.base().is_kernel());
        }
    }

    fn read_thread_names(&mut self) {
        for thread in self.threads.values_mut() {
            if let Some(name) = try_read_thread_name(thread.id) {
                thread.name = Some(name);
            }
        }
    }

    fn read_thread_sctx(&mut self) {
        for thread in self.threads.values_mut() {
            let mut ids = ThreadSctxIds::default();
            thread.sctx = sys_thread_read_sctx_ids(thread.id, &mut ids)
                .ok()
                .map(|()| ids);
        }
    }

    fn resolve_comp_names(&mut self) {
        let mut wanted: Vec<ObjID> = Vec::new();
        for thread in self.threads.values() {
            let Some(sctx) = thread.sctx else { continue };
            for id in [sctx.home, sctx.active] {
                if id.raw() != 0 && !self.comps.contains_key(&id) && !wanted.contains(&id) {
                    wanted.push(id);
                }
            }
        }
        for id in wanted {
            let name = CompartmentHandle::lookup_id(id)
                .ok()
                .and_then(|handle| handle.info().ok().map(|info| info.name));
            self.comps.insert(id, name);
        }
    }

    /// How to label a security context in the display.
    fn comp_label(&self, id: ObjID) -> String {
        if id.raw() == 0 {
            return "[no compartment]".to_string();
        }
        match self.comps.get(&id) {
            Some(Some(name)) => name.clone(),
            // Known-unnamed and never-seen look the same here on purpose: either way the id is
            // all we have to show.
            _ => format!("[{:x}]", id),
        }
    }

    /// The group a thread is displayed under. A zero home means no compartment, which covers
    /// both kernel threads and the userspace ones that belong to none -- the monitor's own,
    /// bootstrap, and statically-linked programs. Only the repr flag separates those.
    fn group_label(&self, thread: &ThreadInfo) -> String {
        if thread.home().raw() == 0 && thread.kernel {
            return "[kernel]".to_string();
        }
        self.comp_label(thread.home())
    }

    /// Threads grouped by home compartment, hottest group first, hottest thread first inside it.
    fn grouped(&self) -> Vec<CompGroup<'_>> {
        let mut by_home: BTreeMap<String, Vec<&ThreadInfo>> = BTreeMap::new();
        for thread in self.sorted() {
            by_home
                .entry(self.group_label(thread))
                .or_default()
                .push(thread);
        }
        let mut groups: Vec<CompGroup<'_>> = by_home
            .into_iter()
            .map(|(label, threads)| CompGroup {
                ticks: threads.iter().map(|t| t.delta_cpu_ticks()).sum(),
                cross: threads.iter().filter(|t| t.is_cross()).count(),
                // Every thread in a group shares a home by construction, so the first answers
                // for all of them.
                system: threads.first().is_some_and(|t| t.home().raw() == 0),
                label,
                threads,
            })
            .collect();
        groups.sort_by(|a, b| {
            b.ticks
                .cmp(&a.ticks)
                .then(b.threads.len().cmp(&a.threads.len()))
                .then(a.label.cmp(&b.label))
        });
        groups
    }

    /// Whether this group's individual thread rows are drawn under the current mode.
    fn show_rows(&self, group: &CompGroup<'_>) -> bool {
        match self.view {
            ThreadView::All => true,
            ThreadView::HideSystem => !group.system,
            ThreadView::HideAll => false,
        }
    }

    /// Where a thread is executing, when that is not its home compartment.
    fn away_label(&self, thread: &ThreadInfo) -> String {
        if !thread.is_cross() {
            return String::new();
        }
        let active = thread.sctx.unwrap().active;
        // A cross thread has a nonzero home by definition, so a zero *active* is not the
        // "belongs to nothing" case the group label describes -- it is a gate call into the
        // monitor, whose instance id is zero.
        if active.raw() == 0 {
            return "-> [monitor]".to_string();
        }
        format!("-> {}", self.comp_label(active))
    }

    /// Threads, most cpu-hungry first.
    fn sorted(&self) -> Vec<&ThreadInfo> {
        let mut visible: Vec<&ThreadInfo> = self.threads.values().collect();
        visible.sort_by(|a, b| {
            b.delta_cpu_ticks()
                .cmp(&a.delta_cpu_ticks())
                .then(b.cpu_ticks().cmp(&a.cpu_ticks()))
                .then(a.id.cmp(&b.id))
        });
        visible
    }

    fn summary(&self) -> String {
        let stats = sys_thread_stats();
        let cpus = sys_info().cpu_count;
        let busy: f64 = self.threads.values().map(|t| t.cpu(self.elapsed)).sum();
        let groups = self.grouped();
        let cross: usize = groups.iter().map(|g| g.cross).sum();
        format!(
            "  —  {} threads ({} running, {} blocked) in {} compartments, {} cross, {:.2}/{} cpus busy, {} ticks/s",
            stats.nr_threads,
            stats.nr_running,
            stats.nr_blocked,
            groups.len(),
            cross,
            busy,
            cpus,
            if self.ticks_per_sec > 0.0 {
                format!("{:.0}", self.ticks_per_sec)
            } else {
                "?".to_string()
            }
        )
    }

    fn time_str(&self, thread: &ThreadInfo) -> String {
        if self.ticks_per_sec <= 0.0 {
            return "-".to_string();
        }
        fmt_time(thread.cpu_ticks() as f64 / self.ticks_per_sec)
    }

    fn render_plain(&self, out: &mut impl Write) -> std::io::Result<()> {
        writeln!(out, "twiztop{}", self.summary())?;
        writeln!(
            out,
            "{:<20}  {:<22}  {:<18}  {:<9}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
            "ID", "NAME", "COMP", "STATE", "CPU%", "USER%", "SYS%", "TIME", "WHERE"
        )?;
        // Flat and one-compartment-per-row rather than grouped: this output is meant to be
        // parsed, and a grouped form would put the compartment on a different line than the
        // thread it describes.
        for group in self.grouped() {
            // Collapsed groups still emit one line, so a parser sees every compartment and its
            // cost whichever mode produced the output.
            if !self.show_rows(&group) {
                writeln!(
                    out,
                    "{:<20}  {:<22}  {:<18.18}  {:<9}  {:>5.1}%  {:>6}  {:>6}  {:>8}  {} thread{}{}",
                    "",
                    "",
                    group.label,
                    "-",
                    group.cpu(self.elapsed) * 100.0,
                    "",
                    "",
                    "",
                    group.threads.len(),
                    if group.threads.len() == 1 { "" } else { "s" },
                    if group.cross > 0 {
                        format!(", {} cross", group.cross)
                    } else {
                        String::new()
                    },
                )?;
                continue;
            }
            for thread in &group.threads {
                writeln!(
                    out,
                    "{:<20.20}  {:<22.22}  {:<18.18}  {:<9.9}  {:>5.1}%  {:>5.1}%  {:>5.1}%  {:>8}  {}",
                    format!("{:x}", thread.id),
                    thread.name.as_deref().unwrap_or(""),
                    group.label,
                    state_str(thread),
                    thread.cpu(self.elapsed) * 100.0,
                    thread.user(self.elapsed) * 100.0,
                    thread.system(self.elapsed) * 100.0,
                    self.time_str(thread),
                    self.away_label(thread),
                )?;
            }
        }
        out.flush()
    }

    fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        let (cols, rows) = size().unwrap_or((80, 24));
        let self_id = sys_thread_self_id();
        // Everything but NAME is fixed width; NAME takes what is left. Thread rows are indented
        // two under their compartment header, and WHERE takes a column at the end, so both come
        // out of NAME's share.
        let name_w = (cols as usize).saturating_sub(81).clamp(8, 32);

        let visible = self.sorted();
        let mut rows_by_state: BTreeMap<&'static str, usize> = BTreeMap::new();
        for thread in &visible {
            *rows_by_state.entry(state_str(thread)).or_default() += 1;
        }

        out.queue(Clear(ClearType::All))?;
        out.queue(MoveTo(0, 0))?;

        out.queue(SetAttribute(Attribute::Bold))?;
        out.queue(SetForegroundColor(Color::Cyan))?;
        out.queue(Print("twiztop"))?;
        out.queue(ResetColor)?;
        out.queue(Print(self.summary()))?;

        out.queue(MoveTo(0, 1))?;
        let mut first = true;
        for (state, count) in &rows_by_state {
            if !first {
                out.queue(Print("  "))?;
            }
            first = false;
            out.queue(SetForegroundColor(state_color(state)))?;
            out.queue(Print(format!("{}: {}", state, count)))?;
            out.queue(ResetColor)?;
        }

        out.queue(MoveTo(0, 3))?;
        out.queue(SetAttribute(Attribute::Reverse))?;
        out.queue(Print(pad(
            &format!(
                "  {:<16}  {:<w$}  {:<9}  {:>6}  {:>6}  {:>6}  {:>8}  {}",
                "ID",
                "NAME",
                "STATE",
                "CPU%",
                "USER%",
                "SYS%",
                "TIME",
                "WHERE",
                w = name_w
            ),
            cols as usize,
        )))?;
        out.queue(SetAttribute(Attribute::Reset))?;

        let body_start: u16 = 4;
        let max_rows = rows.saturating_sub(body_start + 1) as usize;
        // A group costs a header row, so budget rows across groups rather than threads: an
        // overlong first group must not push every other compartment off the screen entirely.
        let mut row = 0usize;
        for group in self.grouped() {
            let show_rows = self.show_rows(&group);
            // A collapsed group needs only its own line; an expanded one is not worth starting
            // with no room for a single thread under it.
            if row + if show_rows { 2 } else { 1 } > max_rows {
                break;
            }
            out.queue(MoveTo(0, body_start + row as u16))?;
            out.queue(SetAttribute(Attribute::Bold))?;
            out.queue(SetForegroundColor(Color::Magenta))?;
            out.queue(Print(group.label.to_string()))?;
            out.queue(ResetColor)?;
            out.queue(SetAttribute(Attribute::Reset))?;
            out.queue(SetForegroundColor(Color::DarkGrey))?;
            out.queue(Print(format!(
                "  {} thread{}",
                group.threads.len(),
                if group.threads.len() == 1 { "" } else { "s" }
            )))?;
            out.queue(ResetColor)?;
            if group.cross > 0 {
                out.queue(SetForegroundColor(Color::Yellow))?;
                out.queue(Print(format!(", {} cross", group.cross)))?;
                out.queue(ResetColor)?;
            }
            // The group's own cpu, so a collapsed group still says what it cost.
            let group_cpu = group.cpu(self.elapsed);
            out.queue(Print("  "))?;
            out.queue(SetForegroundColor(pct_color(group_cpu)))?;
            out.queue(Print(format!("{:.1}%", group_cpu * 100.0)))?;
            out.queue(ResetColor)?;
            row += 1;

            if !show_rows {
                continue;
            }

            for thread in &group.threads {
                if row >= max_rows {
                    break;
                }
                out.queue(MoveTo(0, body_start + row as u16))?;
                row += 1;

                if thread.id == self_id {
                    out.queue(SetAttribute(Attribute::Bold))?;
                }
                out.queue(Print(format!(
                    "  {:<16.16}  {:<w$.w$}  ",
                    format!("{:x}", thread.id),
                    thread.name.as_deref().unwrap_or(""),
                    w = name_w
                )))?;
                out.queue(SetForegroundColor(state_color(state_str(thread))))?;
                out.queue(Print(format!("{:<9.9}", state_str(thread))))?;
                out.queue(ResetColor)?;

                for frac in [
                    thread.cpu(self.elapsed),
                    thread.user(self.elapsed),
                    thread.system(self.elapsed),
                ] {
                    out.queue(Print("  "))?;
                    out.queue(SetForegroundColor(pct_color(frac)))?;
                    out.queue(Print(format!("{:>5.1}%", frac * 100.0)))?;
                    out.queue(ResetColor)?;
                }
                out.queue(Print(format!("  {:>8}  ", self.time_str(thread))))?;
                if thread.is_cross() {
                    out.queue(SetForegroundColor(Color::Yellow))?;
                    out.queue(Print(self.away_label(thread)))?;
                    out.queue(ResetColor)?;
                }
                out.queue(SetAttribute(Attribute::Reset))?;
            }
        }

        out.queue(MoveTo(0, rows.saturating_sub(1)))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print(format!(
            "q/Esc: quit  |  t: {}  |  grouped by home compartment, cpu (desc)  |  WHERE = running in another compartment",
            self.view.label(),
        )))?;
        out.queue(ResetColor)?;

        out.flush()
    }
}

/// Thread names are mirrored into notes on the thread's repr object (see the reference
/// runtime's `set_name`), so that is the only place to read one from. The most recently
/// added note is the most specific label the thread has given itself.
fn try_read_thread_name(id: ObjID) -> Option<String> {
    let mut last_key = None;
    let mut offset = 0;
    loop {
        let mut keys = [0u64; 16];
        let n = sys_object_enumerate_notes(id, offset, &mut keys).ok()?;
        if n == 0 {
            break;
        }
        last_key = Some(keys[n - 1]);
        offset += n;
        if n < keys.len() {
            break;
        }
    }

    let mut buf = [0u8; 128];
    let len = sys_object_get_note(id, last_key?, &mut buf).ok()?;
    let s = std::str::from_utf8(&buf[..len.min(buf.len())]).ok()?;
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn state_str(thread: &ThreadInfo) -> &'static str {
    if thread.err.is_some() {
        return "?";
    }
    match thread.state {
        Some(ExecutionState::Running) => "Running",
        Some(ExecutionState::Sleeping) => "Sleeping",
        Some(ExecutionState::Suspended) => "Suspended",
        Some(ExecutionState::Exited) => "Exited",
        None => "?",
    }
}

fn state_color(state: &str) -> Color {
    match state {
        "Running" => Color::Green,
        "Sleeping" => Color::Blue,
        "Exited" => Color::DarkGrey,
        "?" => Color::DarkGrey,
        _ => Color::Yellow,
    }
}

fn pct_color(frac: f64) -> Color {
    match frac {
        f if f >= 0.75 => Color::Red,
        f if f >= 0.40 => Color::Yellow,
        f if f > 0.0 => Color::Green,
        _ => Color::DarkGrey,
    }
}

fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0:00.0".to_string();
    }
    let mins = (secs / 60.0).floor();
    format!("{}:{:04.1}", mins as u64, secs - mins * 60.0)
}

fn pad(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    if s.len() < width {
        s.push_str(&" ".repeat(width - s.len()));
    }
    s
}
