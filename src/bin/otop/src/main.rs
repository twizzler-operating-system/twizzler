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
use twizzler_abi::{
    object::ObjID,
    syscall::{
        EnumerateKind, MemoryStats, ObjectInfo, sys_enumerate, sys_info, sys_memory_stats,
        sys_object_enumerate_notes, sys_object_get_note, sys_object_stat, sys_object_stats,
    },
};

const REFRESH: Duration = Duration::from_millis(2000);

#[derive(Copy, Clone, PartialEq, Eq)]
enum SortKey {
    Pages,
    Delta,
    Maps,
    Ties,
    Name,
    Id,
}

impl SortKey {
    const ALL: [SortKey; 6] = [
        SortKey::Pages,
        SortKey::Delta,
        SortKey::Maps,
        SortKey::Ties,
        SortKey::Name,
        SortKey::Id,
    ];

    fn name(&self) -> &'static str {
        match self {
            SortKey::Pages => "pages",
            SortKey::Delta => "delta",
            SortKey::Maps => "maps",
            SortKey::Ties => "ties",
            SortKey::Name => "name",
            SortKey::Id => "id",
        }
    }

    /// Numeric keys read best largest-first, text keys smallest-first.
    fn descending(&self) -> bool {
        !matches!(self, SortKey::Name | SortKey::Id)
    }

    fn step(&self, forward: bool) -> SortKey {
        let idx = Self::ALL.iter().position(|k| k == self).unwrap_or(0);
        let n = Self::ALL.len();
        Self::ALL[if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        }]
    }
}

fn main() {
    enable_raw_mode().unwrap();
    let mut out = stdout();
    out.execute(EnterAlternateScreen).unwrap();
    out.execute(Hide).unwrap();

    let mut tracker = ObjectTracker::default();

    loop {
        tracker.scan();
        tracker.read_stats();
        tracker.read_names();
        tracker.render(&mut out).unwrap();

        if wait_for_input(REFRESH, &mut tracker, &mut out) {
            break;
        }
    }

    let _ = out.execute(Show);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Waits out the refresh interval, returning true if the user asked to quit. Sort changes are
/// applied and redrawn immediately rather than at the next scan, so the display tracks the key.
fn wait_for_input(timeout: Duration, tracker: &mut ObjectTracker, out: &mut impl Write) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        if event::poll(deadline - now).unwrap() {
            if let Event::Key(key) = event::read().unwrap() {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                let mut resort = true;
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return true,
                    KeyCode::Char('s') | KeyCode::Right => {
                        tracker.sort = tracker.sort.step(true);
                        tracker.reverse = false;
                    }
                    KeyCode::Char('S') | KeyCode::Left => {
                        tracker.sort = tracker.sort.step(false);
                        tracker.reverse = false;
                    }
                    KeyCode::Char('r') => tracker.reverse = !tracker.reverse,
                    _ => resort = false,
                }
                if resort {
                    tracker.render(out).unwrap();
                }
            }
        }
    }
}

struct ObjEntry {
    id: ObjID,
    info: ObjectInfo,
    name: Option<String>,
    prev_pages: usize,
}

impl ObjEntry {
    fn delta(&self) -> isize {
        self.info.pages as isize - self.prev_pages as isize
    }

    fn ties(&self) -> usize {
        self.info.ties_to + self.info.ties_from
    }
}

struct ObjectTracker {
    objects: BTreeMap<ObjID, ObjEntry>,
    sort: SortKey,
    reverse: bool,
}

impl Default for ObjectTracker {
    fn default() -> Self {
        Self {
            objects: BTreeMap::new(),
            sort: SortKey::Pages,
            reverse: false,
        }
    }
}

impl ObjectTracker {
    fn scan(&mut self) {
        let mut buf = [ObjID::default(); 256];
        let mut offset = 0;

        loop {
            match sys_enumerate(EnumerateKind::Objects, &mut buf, offset) {
                Ok(count) => {
                    if count == 0 {
                        break;
                    }
                    for i in 0..count {
                        let id = buf[i as usize];
                        self.objects.entry(id).or_insert(ObjEntry {
                            id,
                            info: ObjectInfo {
                                id,
                                maps: 0,
                                ties_to: 0,
                                ties_from: 0,
                                life: twizzler_abi::syscall::LifetimeType::Volatile,
                                backing: twizzler_abi::syscall::BackingType::Normal,
                                pages: 0,
                            },
                            name: None,
                            prev_pages: 0,
                        });
                    }
                    offset += count;
                }
                Err(e) => {
                    eprintln!("enumerate error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn read_stats(&mut self) {
        let mut dead = Vec::new();
        for entry in self.objects.values_mut() {
            match sys_object_stat(entry.id) {
                Ok(info) => {
                    entry.prev_pages = entry.info.pages;
                    entry.info = info;
                }
                Err(_) => {
                    dead.push(entry.id);
                }
            }
        }
        for id in dead {
            self.objects.remove(&id);
        }
    }

    fn read_names(&mut self) {
        for entry in self.objects.values_mut() {
            if entry.name.is_some() {
                continue;
            }
            entry.name = try_read_object_name(entry.id);
        }
    }

    fn sorted(&self) -> Vec<&ObjEntry> {
        let mut sorted: Vec<&ObjEntry> = self.objects.values().collect();
        sorted.sort_by(|a, b| {
            let ord = match self.sort {
                SortKey::Pages => a.info.pages.cmp(&b.info.pages),
                SortKey::Delta => a.delta().cmp(&b.delta()),
                SortKey::Maps => a.info.maps.cmp(&b.info.maps),
                SortKey::Ties => a.ties().cmp(&b.ties()),
                // Unnamed objects sort last regardless of direction: they carry no key.
                SortKey::Name => match (a.name.as_deref(), b.name.as_deref()) {
                    (Some(a), Some(b)) => a.cmp(b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                SortKey::Id => a.id.cmp(&b.id),
            };
            let ord = if self.sort.descending() != self.reverse {
                ord.reverse()
            } else {
                ord
            };
            ord.then(a.id.cmp(&b.id))
        });
        sorted
    }

    fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        let (cols, rows) = size().unwrap_or((80, 24));
        let global = sys_object_stats();
        let mem = sys_memory_stats();
        let visible = self.sorted();
        let total_maps: usize = visible.iter().map(|e| e.info.maps).sum();

        out.queue(Clear(ClearType::All))?;
        out.queue(MoveTo(0, 0))?;

        out.queue(SetAttribute(Attribute::Bold))?;
        out.queue(SetForegroundColor(Color::Cyan))?;
        out.queue(Print("otop"))?;
        out.queue(ResetColor)?;
        out.queue(Print(format!(
            "  —  {} objects, {} mapped, {} mappings, {} handles, {} ties, {} pending delete",
            global.nr_objects,
            global.nr_mapped,
            total_maps,
            global.nr_handles,
            global.nr_ties,
            global.nr_pending_delete
        )))?;

        out.queue(MoveTo(0, 1))?;
        self.render_mem(out, &mem)?;

        out.queue(MoveTo(0, 3))?;
        out.queue(SetAttribute(Attribute::Reverse))?;
        out.queue(Print(pad(
            &format!(
                "{:<20}  {:<32}  {:>8}  {:>8}  {:>5}  {:>5}",
                self.col("ID", SortKey::Id),
                self.col("NAME", SortKey::Name),
                self.col("PAGES", SortKey::Pages),
                self.col("DELTA", SortKey::Delta),
                self.col("MAPS", SortKey::Maps),
                self.col("TIES", SortKey::Ties),
            ),
            cols as usize,
        )))?;
        out.queue(SetAttribute(Attribute::Reset))?;

        let body_start: u16 = 4;
        let max_rows = rows.saturating_sub(body_start + 1) as usize;
        for (i, entry) in visible.iter().take(max_rows).enumerate() {
            let name = entry.name.as_deref().unwrap_or("");
            let delta = entry.delta();

            out.queue(MoveTo(0, body_start + i as u16))?;
            out.queue(Print(format!(
                "{:<20.20}  {:<32.32}  {:>8}  ",
                format!("{:x}", entry.id),
                name,
                entry.info.pages
            )))?;

            if delta != 0 {
                out.queue(SetForegroundColor(if delta > 0 {
                    Color::Red
                } else {
                    Color::Green
                }))?;
                out.queue(Print(format!("{:>+8}", delta)))?;
                out.queue(ResetColor)?;
            } else {
                out.queue(Print(format!("{:>8}", delta)))?;
            }

            out.queue(Print(format!(
                "  {:>5}  {:>5}",
                entry.info.maps,
                entry.ties()
            )))?;
        }

        out.queue(MoveTo(0, rows.saturating_sub(1)))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print(format!(
            "q/Esc: quit  |  s/S or ←/→: sort key ({})  |  r: reverse ({})  |  delta = pages change since last scan",
            self.sort.name(),
            if self.sort.descending() != self.reverse {
                "desc"
            } else {
                "asc"
            }
        )))?;
        out.queue(ResetColor)?;

        out.flush()
    }

    /// System memory, as the frame tracker sees it. `page_data` is what the objects listed below
    /// are made of; `kernel_used` and the kalloc totals are everything else the kernel holds.
    fn render_mem(&self, out: &mut impl Write, mem: &MemoryStats) -> std::io::Result<()> {
        let page = if mem.nr_levels > 0 {
            mem.levels[0].page_size
        } else {
            sys_info().page_size()
        };
        let total = if mem.tracker.total > 0 {
            mem.tracker.total * page
        } else {
            mem.total_bytes()
        };
        let free = mem.free_bytes();
        let used = total.saturating_sub(free);
        let frac = if total > 0 {
            used as f64 / total as f64
        } else {
            0.0
        };

        out.queue(Print(format!("mem   {} total, used ", fmt_bytes(total))))?;
        out.queue(SetForegroundColor(pct_color(frac)))?;
        out.queue(Print(format!("{} ({:.1}%)", fmt_bytes(used), frac * 100.0)))?;
        out.queue(ResetColor)?;
        out.queue(Print(format!(
            ", free {}  |  objects {}, kernel {}, kalloc {}, pager {}",
            fmt_bytes(free),
            fmt_bytes(mem.tracker.page_data * page),
            fmt_bytes(mem.tracker.kernel_used * page),
            fmt_bytes(mem.kalloc_bytes()),
            fmt_bytes(mem.tracker.pager_outstanding * page),
        )))?;
        Ok(())
    }

    fn col(&self, name: &str, key: SortKey) -> String {
        if self.sort == key {
            format!("{}*", name)
        } else {
            name.to_string()
        }
    }
}

fn try_read_object_name(id: ObjID) -> Option<String> {
    let mut keys = [0u64; 16];
    let n = sys_object_enumerate_notes(id, 0, &mut keys).ok()?;
    if n == 0 {
        return None;
    }
    let key = keys[0];
    let mut buf = [0u8; 128];
    let len = sys_object_get_note(id, key, &mut buf).ok()?;
    if len == 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..len.min(128)]).ok()?;
    Some(s.to_string())
}

fn pct_color(frac: f64) -> Color {
    match frac {
        f if f >= 0.90 => Color::Red,
        f if f >= 0.75 => Color::Yellow,
        _ => Color::Green,
    }
}

fn fmt_bytes(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut val = bytes as f64;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{}{}", bytes, UNITS[unit])
    } else {
        format!("{:.1}{}", val, UNITS[unit])
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    if s.len() < width {
        s.push_str(&" ".repeat(width - s.len()));
    }
    s
}
