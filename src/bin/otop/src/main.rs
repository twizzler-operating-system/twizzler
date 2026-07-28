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
use twizzler_abi::{
    object::ObjID,
    syscall::{
        EnumerateKind, ObjectInfo, sys_enumerate, sys_object_enumerate_notes, sys_object_get_note,
        sys_object_stat, sys_object_stats,
    },
};

const REFRESH: Duration = Duration::from_millis(2000);

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

struct ObjEntry {
    id: ObjID,
    info: ObjectInfo,
    name: Option<String>,
    prev_pages: usize,
}

#[derive(Default)]
struct ObjectTracker {
    objects: BTreeMap<ObjID, ObjEntry>,
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

    fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        let (cols, rows) = size().unwrap_or((80, 24));
        let global = sys_object_stats();

        let mut sorted: Vec<&ObjEntry> = self.objects.values().collect();
        sorted.sort_by(|a, b| b.info.pages.cmp(&a.info.pages).then(a.id.cmp(&b.id)));

        out.queue(Clear(ClearType::All))?;
        out.queue(MoveTo(0, 0))?;

        out.queue(SetAttribute(Attribute::Bold))?;
        out.queue(SetForegroundColor(Color::Cyan))?;
        out.queue(Print("otop"))?;
        out.queue(ResetColor)?;
        out.queue(Print(format!(
            "  —  {} objects, {} mapped, {} pending delete",
            global.nr_objects, global.nr_mapped, global.nr_pending_delete
        )))?;

        out.queue(MoveTo(0, 2))?;
        out.queue(SetAttribute(Attribute::Reverse))?;
        out.queue(Print(pad(
            &format!(
                "{:<20}  {:<32}  {:>8}  {:>8}  {:>5}  {:>5}",
                "ID", "NAME", "PAGES", "DELTA", "MAPS", "TIES"
            ),
            cols as usize,
        )))?;
        out.queue(SetAttribute(Attribute::Reset))?;

        let body_start: u16 = 3;
        let max_rows = rows.saturating_sub(body_start + 1) as usize;
        for (i, entry) in sorted.iter().take(max_rows).enumerate() {
            let name = entry.name.as_deref().unwrap_or("");
            let delta = entry.info.pages as isize - entry.prev_pages as isize;

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
                entry.info.ties_to + entry.info.ties_from
            )))?;
        }

        out.queue(MoveTo(0, rows.saturating_sub(1)))?;
        out.queue(SetForegroundColor(Color::DarkGrey))?;
        out.queue(Print(
            "q/Esc: quit  |  sorted by pages (desc)  |  delta = pages change since last scan",
        ))?;
        out.queue(ResetColor)?;

        out.flush()
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

fn pad(s: &str, width: usize) -> String {
    let mut s = s.to_string();
    if s.len() < width {
        s.push_str(&" ".repeat(width - s.len()));
    }
    s
}
