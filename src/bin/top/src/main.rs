use std::{
    collections::BTreeMap,
    io::{Write, stdout},
    time::Duration,
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
use twizzler::object::TypedObject;
use twizzler_abi::{
    object::ObjID,
    syscall::{EnumerateKind, ThreadSchedStats, sys_thread_read_stats, sys_thread_self_id},
    thread::{ExecutionState, ThreadRepr},
};
use twizzler_rt_abi::{error::TwzError, object::MapFlags};

const REFRESH: Duration = Duration::from_millis(1000);

fn main() {
    enable_raw_mode().unwrap();
    let mut out = stdout();
    out.execute(EnterAlternateScreen).unwrap();
    out.execute(Hide).unwrap();

    let mut tracker = ThreadTracker::default();

    loop {
        tracker.scan_for_threads();
        tracker.read_thread_names();
        tracker.read_thread_stats();

        let quit = {
            tracker.render(&mut out).unwrap();
            wait_for_quit(REFRESH)
        };

        if quit {
            break;
        }
    }

    let _ = out.execute(Show);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn wait_for_quit(timeout: Duration) -> bool {
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
                    _ => {}
                }
            }
        }
    }
}

struct ThreadInfo {
    id: ObjID,
    name: Option<String>,
    state: ExecutionState,
    err: Option<TwzError>,
    stats: ThreadSchedStats,
}

impl ThreadInfo {
    fn calc_stats(&self) -> (f64, f64, f64) {
        let total = self.stats.idle + self.stats.system + self.stats.user;
        if total == 0 {
            return (0.0, 0.0, 0.0);
        }
        (
            self.stats.idle as f64 / total as f64,
            self.stats.system as f64 / total as f64,
            self.stats.user as f64 / total as f64,
        )
    }
}

#[derive(Default)]
struct ThreadTracker {
    threads: BTreeMap<ObjID, ThreadInfo>,
}

impl ThreadTracker {
    fn add_thread(&mut self, id: ObjID) {
        let info = ThreadInfo {
            id,
            name: None,
            state: ExecutionState::Running,
            err: None,
            stats: ThreadSchedStats::default(),
        };
        self.threads.insert(id, info);
    }

    fn get_thread_info(&self, id: &ObjID) -> Option<&ThreadInfo> {
        self.threads.get(id)
    }

    fn read_thread_stats(&mut self) {
        for thread_info in self.threads.iter_mut() {
            let handle =
                twizzler::object::Object::<ThreadRepr>::map(*thread_info.0, MapFlags::READ);
            let state = handle.map(|h| h.base().get_state());
            match state {
                Ok(s) => {
                    thread_info.1.state = s;
                }
                Err(e) => {
                    thread_info.1.err = Some(e);
                }
            }
            let stats = sys_thread_read_stats(*thread_info.0, &mut thread_info.1.stats);
            if let Err(e) = stats {
                thread_info.1.err = Some(e);
            }
        }

        self.threads.retain(|_, t| t.err.is_none());
    }

    fn scan_for_threads(&mut self) {
        let mut buf = [ObjID::default(); 128];
        let mut offset = 0;

        loop {
            match twizzler_abi::syscall::sys_enumerate(EnumerateKind::Threads, &mut buf, offset) {
                Ok(count) => {
                    if count == 0 {
                        break;
                    }

                    for i in 0..count {
                        let thread_id = buf[i as usize];

                        if self.get_thread_info(&thread_id).is_none() {
                            self.add_thread(thread_id);
                        }
                    }

                    offset += count;
                }
                Err(e) => {
                    eprintln!("Error enumerating threads: {:?}", e);
                    break;
                }
            }
        }
    }

    fn read_thread_names(&mut self) {
        for thread_info in self.threads.values_mut() {
            if thread_info.name.is_none() {
                // TODO
                thread_info.name = Some(format!("Thread-{}", thread_info.id));
            }
        }
    }

    fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        let (cols, rows) = size().unwrap_or((80, 24));
        let self_id = sys_thread_self_id();

        let mut rows_by_state: BTreeMap<ExecutionState, usize> = BTreeMap::new();
        let mut visible: Vec<&ThreadInfo> = Vec::new();
        for t in self.threads.values() {
            if t.err.is_some() {
                continue;
            }
            *rows_by_state.entry(t.state).or_default() += 1;
            visible.push(t);
        }
        visible.sort_by(|a, b| {
            b.calc_stats()
                .2
                .total_cmp(&a.calc_stats().2)
                .then(a.id.cmp(&b.id))
        });

        out.queue(Clear(ClearType::All))?;
        out.queue(MoveTo(0, 0))?;

        out.queue(SetAttribute(Attribute::Bold))?;
        out.queue(SetForegroundColor(Color::Cyan))?;
        out.queue(Print("twiztop"))?;
        out.queue(ResetColor)?;
        out.queue(Print(format!(
            "  —  {} threads, self = {:x}",
            self.threads.len(),
            self_id
        )))?;

        out.queue(MoveTo(0, 1))?;
        let mut first = true;
        for (state, count) in &rows_by_state {
            if !first {
                out.queue(Print("  "))?;
            }
            first = false;
            out.queue(SetForegroundColor(state_color(*state)))?;
            out.queue(Print(format!("{:?}: {}", state, count)))?;
            out.queue(ResetColor)?;
        }

        out.queue(MoveTo(0, 3))?;
        out.queue(SetAttribute(Attribute::Reverse))?;
        out.queue(Print(pad(
            &format!(
                "{:<20}  {:<24}  {:<10}  {:>7}  {:>7}  {:>7}",
                "ID", "NAME", "STATE", "USER%", "SYS%", "IDLE%"
            ),
            cols as usize,
        )))?;
        out.queue(SetAttribute(Attribute::Reset))?;

        let body_start: u16 = 4;
        let max_rows = rows.saturating_sub(body_start + 1) as usize;
        for (i, t) in visible.iter().take(max_rows).enumerate() {
            let (idle, system, user) = t.calc_stats();
            let name = t.name.as_deref().unwrap_or("<unknown>");
            out.queue(MoveTo(0, body_start + i as u16))?;

            if t.id == self_id {
                out.queue(SetAttribute(Attribute::Bold))?;
            }
            out.queue(Print(format!(
                "{:<20.20}  {:<24.24}  ",
                format!("{:x}", t.id),
                name
            )))?;
            out.queue(SetForegroundColor(state_color(t.state)))?;
            out.queue(Print(format!("{:<10.10}", format!("{:?}", t.state))))?;
            out.queue(ResetColor)?;
            out.queue(Print("  "))?;
            out.queue(SetForegroundColor(pct_color(user)))?;
            out.queue(Print(format!("{:>6.1}%", user * 100.0)))?;
            out.queue(ResetColor)?;
            out.queue(Print("  "))?;
            out.queue(SetForegroundColor(pct_color(system)))?;
            out.queue(Print(format!("{:>6.1}%", system * 100.0)))?;
            out.queue(ResetColor)?;
            out.queue(Print(format!("  {:>6.1}%", idle * 100.0)))?;
            out.queue(SetAttribute(Attribute::Reset))?;
        }

        out.queue(MoveTo(0, rows.saturating_sub(1)))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print("q/Esc: quit"))?;
        out.queue(ResetColor)?;

        out.flush()
    }
}

fn state_color(state: ExecutionState) -> Color {
    match state {
        ExecutionState::Running => Color::Green,
        ExecutionState::Sleeping => Color::Blue,
        ExecutionState::Exited => Color::DarkGrey,
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

fn pad(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    if s.len() < width {
        s.push_str(&" ".repeat(width - s.len()));
    }
    s
}
