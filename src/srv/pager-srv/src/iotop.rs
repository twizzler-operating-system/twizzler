use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
    fmt::Write,
};

use twizzler::object::ObjID;
use crate::helpers::PAGE;

#[derive(Clone, Debug, PartialEq)]
pub struct IOSample {
    pub timestamp: Instant,
    pub pages_read: usize,
    pub pages_written: usize,
    pub read_bytes_per_sec: f64,
    pub write_bytes_per_sec: f64,
}

#[derive(Clone, Debug)]
pub struct ProcessIOStats {
    pub obj_id: ObjID,
    pub total_read: usize,
    pub total_written: usize,
    pub samples: VecDeque<IOSample>,
    pub last_update: Instant,
}

impl ProcessIOStats {
    pub fn new(obj_id: ObjID) -> Self {
        Self {
            obj_id,
            total_read: 0,
            total_written: 0,
            samples: VecDeque::with_capacity(60), 
            last_update: Instant::now(),
        }
    }

    pub fn add_io(&mut self, read_pages: usize, written_pages: usize) {
        self.total_read += read_pages;
        self.total_written += written_pages;
        
        let now = Instant::now();
        
        let dt = now.checked_duration_since(self.last_update)
            .unwrap_or(Duration::from_millis(1));            
        let sample = IOSample {
            timestamp: now,
            pages_read: read_pages,
            pages_written: written_pages,
            read_bytes_per_sec: if dt.as_secs_f64() > 0.0 {
                (read_pages * PAGE as usize) as f64 / dt.as_secs_f64()
            } else {
                0.0
            },
            write_bytes_per_sec: if dt.as_secs_f64() > 0.0 {
                (written_pages * PAGE as usize) as f64 / dt.as_secs_f64()
            } else {
                0.0
            },
        };
        
        self.samples.push_back(sample);
        if self.samples.len() > 60 {
            self.samples.pop_front();
        }
        
        self.last_update = now;
    }
    
    pub fn current_read_bps(&self) -> f64 {
        self.samples.back().map(|s| s.read_bytes_per_sec).unwrap_or(0.0)
    }
    
    pub fn current_write_bps(&self) -> f64 {
        self.samples.back().map(|s| s.write_bytes_per_sec).unwrap_or(0.0)
    }
    
    pub fn avg_read_bps(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.samples.iter().map(|s| s.read_bytes_per_sec).sum();
        sum / self.samples.len() as f64
    }
    
    pub fn avg_write_bps(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.samples.iter().map(|s| s.write_bytes_per_sec).sum();
        sum / self.samples.len() as f64
    }
}

#[derive(Debug)]
pub struct PagerIOTop {
    processes: HashMap<ObjID, ProcessIOStats>,
    start_time: Instant,
    last_display: Instant,
    total_read_bytes: u64,
    total_written_bytes: u64,
}

impl Default for PagerIOTop {
    fn default() -> Self {
        Self::new()
    }
}

impl PagerIOTop {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            processes: HashMap::new(),
            start_time: now,
            last_display: now,
            total_read_bytes: 0,
            total_written_bytes: 0,
        }
    }
    
    pub fn record_io(&mut self, obj_id: ObjID, read_pages: usize, written_pages: usize) {
        let stats = self.processes.entry(obj_id).or_insert_with(|| ProcessIOStats::new(obj_id));
        stats.add_io(read_pages, written_pages);
        
        self.total_read_bytes += (read_pages * PAGE as usize) as u64;
        self.total_written_bytes += (written_pages * PAGE as usize) as u64;
    }
    
    pub fn cleanup_old_entries(&mut self) {
        let now = Instant::now();
        let cutoff_duration = Duration::from_secs(300); 
        
        self.processes.retain(|_, stats| {
            match now.checked_duration_since(stats.last_update) {
                Some(elapsed) => elapsed < cutoff_duration,
                None => {
                    // If we can't calculate duration (clock went backwards), 
                    // keep the entry but update its timestamp
                    // This can happen during system startup
                    true
                }
            }
        });
    }   
    fn format_bytes(bytes: f64) -> String {
        const UNITS: &[&str] = &["B/s", "KB/s", "MB/s", "GB/s"];
        let mut size = bytes;
        let mut unit_idx = 0;
        
        while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
            size /= 1024.0;
            unit_idx += 1;
        }
        
        if size >= 100.0 {
            format!("{:.0} {}", size, UNITS[unit_idx])
        } else if size >= 10.0 {
            format!("{:.1} {}", size, UNITS[unit_idx])
        } else {
            format!("{:.2} {}", size, UNITS[unit_idx])
        }
    }
    
    fn format_total_bytes(bytes: u64) -> String {
        const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
        let mut size = bytes as f64;
        let mut unit_idx = 0;
        
        while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
            size /= 1024.0;
            unit_idx += 1;
        }
        
        if size >= 100.0 {
            format!("{:.0} {}", size, UNITS[unit_idx])
        } else {
            format!("{:.1} {}", size, UNITS[unit_idx])
        }
    }
    
    pub fn display(&mut self) -> String {
        let now = Instant::now();
        let uptime = now.duration_since(self.start_time);
        
        let since_last = now.checked_duration_since(self.last_display)
            .unwrap_or(Duration::from_secs(0));
        
        self.last_display = now;
        self.cleanup_old_entries();
        
        let mut output = String::new();
        
        writeln!(output, "\x1B[2J\x1B[H").unwrap();
        
        writeln!(output, "Twizzler Pager I/O Top - {} processes", self.processes.len()).unwrap();
        writeln!(output, "Uptime: {:.1}s, Update: {:.1}s ago", 
                uptime.as_secs_f64(), since_last.as_secs_f64()).unwrap();

        writeln!(output, "Total: {} read, {} written", 
                Self::format_total_bytes(self.total_read_bytes),
                Self::format_total_bytes(self.total_written_bytes)).unwrap();
        
        let total_current_read: f64 = self.processes.values()
            .map(|p| p.current_read_bps())
            .sum();
        let total_current_write: f64 = self.processes.values()
            .map(|p| p.current_write_bps())
            .sum();
            
        writeln!(output, "Current: {} read, {} write",
                Self::format_bytes(total_current_read),
                Self::format_bytes(total_current_write)).unwrap();
        writeln!(output).unwrap();
        
        writeln!(output, "{:<18} {:>10} {:>10} {:>12} {:>12} {:>12} {:>12}",
                "OBJECT_ID", "TOTAL_READ", "TOTAL_WRITE", "READ/s", "WRITE/s", "AVG_READ/s", "AVG_WRITE/s").unwrap();
        writeln!(output, "{}", "-".repeat(98)).unwrap();
        
        let mut sorted_processes: Vec<_> = self.processes.values().collect();
        sorted_processes.sort_by(|a, b| {
            let a_io = a.current_read_bps() + a.current_write_bps();
            let b_io = b.current_read_bps() + b.current_write_bps();
            b_io.partial_cmp(&a_io).unwrap_or(std::cmp::Ordering::Equal)
        });
        
        for stats in sorted_processes.iter().take(20) {
            let obj_id_str = format!("{:016x}", stats.obj_id.raw());
            writeln!(output, "{:<18} {:>10} {:>10} {:>12} {:>12} {:>12} {:>12}",
                    obj_id_str,
                    Self::format_total_bytes(stats.total_read as u64 * PAGE),
                    Self::format_total_bytes(stats.total_written as u64 * PAGE),
                    Self::format_bytes(stats.current_read_bps()),
                    Self::format_bytes(stats.current_write_bps()),
                    Self::format_bytes(stats.avg_read_bps()),
                    Self::format_bytes(stats.avg_write_bps())).unwrap();
        }
        
        writeln!(output).unwrap();
        writeln!(output, "Press Ctrl+C to exit").unwrap();
        
        output
    }
    
    pub fn get_top_io_objects(&self, count: usize) -> Vec<(ObjID, f64, f64)> {
        let mut objects: Vec<_> = self.processes.iter()
            .map(|(id, stats)| (*id, stats.current_read_bps(), stats.current_write_bps()))
            .collect();
        
        objects.sort_by(|a, b| {
            let a_total = a.1 + a.2;
            let b_total = b.1 + b.2;
            b_total.partial_cmp(&a_total).unwrap_or(std::cmp::Ordering::Equal)
        });
        
        objects.into_iter().take(count).collect()
    }

}

