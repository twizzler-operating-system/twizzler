use std::{
    env,
    thread,
    time::Duration,
    io::{self, Write},
    process,
};

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{SystemTime, UNIX_EPOCH};
use pager_srv::{get_object_pager_data, get_nth_iotop_object_id, iotop::PagerIotopData};
use twizzler::object::ObjID;

struct IOTopConfig {
    refresh_rate: u64,
    batch_mode: bool,
    count: Option<usize>,
    delay: u64,
}

impl Default for IOTopConfig {
    fn default() -> Self {
        Self {
            refresh_rate: 1000, // 1 second
            batch_mode: false,
            count: None,
            delay: 0,
        }
    }
}

fn parse_args() -> IOTopConfig {
    let args: Vec<String> = env::args().collect();
    let mut config = IOTopConfig::default();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-d" | "--delay" => {
                if i + 1 < args.len() {
                    config.refresh_rate = args[i + 1].parse().unwrap_or(1000);
                    i += 1;
                }
            }
            "-b" | "--batch" => {
                config.batch_mode = true;
            }
            "-n" | "--count" => {
                if i + 1 < args.len() {
                    config.count = Some(args[i + 1].parse().unwrap_or(10));
                    i += 1;
                }
            }
            "-h" | "--help" => {
                print_help();
                process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    config
}

fn print_help() {
    println!("pager-iotop - Monitor Twizzler pager I/O activity");
    println!();
    println!("Usage: pager-iotop [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -d, --delay N     Refresh delay in milliseconds (default: 1000)");
    println!("  -b, --batch       Batch mode (no screen clearing)");
    println!("  -n, --count N     Number of iterations in batch mode");
    println!("  -h, --help        Show this help");
    println!();
    println!("Interactive keys:");
    println!("  q, Ctrl+C        Quit");
    println!("  r                Refresh now");
    println!("  +                Increase refresh rate");
    println!("  -                Decrease refresh rate");
}

fn get_all_iotop_objs() -> Result<Vec<ObjID>, Box<dyn std::error::Error>> {
    let mut obj_ids = Vec::new();
    let mut n = 0;
    
    // Keep getting object IDs until we get None
    while let Ok(Some(obj_id)) = get_nth_iotop_object_id(n) {
        obj_ids.push(obj_id);
        n += 1;
    }
    
    Ok(obj_ids)
}

fn get_all_iotop_data() -> Result<Vec<PagerIotopData>, Box<dyn std::error::Error>> {
    let obj_ids = get_all_iotop_objs()?;
    let mut data_list = Vec::new();
    
    for obj_id in obj_ids {
        if let Ok(Some(data)) = get_object_pager_data(obj_id) {
            data_list.push(data);
        }
    }
    
    Ok(data_list)
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

// Add function to format the display from PagerIotopData list
fn format_iotop_display(data_list: &[PagerIotopData]) -> String {
    use std::fmt::Write;
    let mut output = String::new();
    
    writeln!(output, "\x1B[2J\x1B[H").unwrap();
    writeln!(output, "Twizzler Pager I/O Top - {} processes", data_list.len()).unwrap();
    
    let total_read_bytes: u64 = data_list.iter().map(|d| d.total_read as u64 * 4096).sum(); // Assuming PAGE = 4096
    let total_written_bytes: u64 = data_list.iter().map(|d| d.total_written as u64 * 4096).sum();
    let total_current_read: f64 = data_list.iter().map(|d| d.current_read_bps).sum();
    let total_current_write: f64 = data_list.iter().map(|d| d.current_write_bps).sum();
    
    writeln!(output, "Total: {} read, {} written", 
            format_total_bytes(total_read_bytes),
            format_total_bytes(total_written_bytes)).unwrap();
    
    writeln!(output, "Current: {} read, {} write",
            format_bytes(total_current_read),
            format_bytes(total_current_write)).unwrap();
    writeln!(output).unwrap();
    
    writeln!(output, "{:<18} {:>10} {:>10} {:>12} {:>12} {:>12} {:>12}",
            "OBJECT_ID", "TOTAL_READ", "TOTAL_WRITE", "READ/s", "WRITE/s", "AVG_READ/s", "AVG_WRITE/s").unwrap();
    writeln!(output, "{}", "-".repeat(98)).unwrap();
    
    for data in data_list.iter().take(20) {
        let obj_id_str = format!("{:016x}", data.obj_id.raw());
        writeln!(output, "{:<18} {:>10} {:>10} {:>12} {:>12} {:>12} {:>12}",
                obj_id_str,
                format_total_bytes(data.total_read as u64 * 4096),
                format_total_bytes(data.total_written as u64 * 4096),
                format_bytes(data.current_read_bps),
                format_bytes(data.current_write_bps),
                format_bytes(data.avg_read_bps),
                format_bytes(data.avg_write_bps)).unwrap();
    }
    
    writeln!(output).unwrap();
    writeln!(output, "Press Ctrl+C to exit").unwrap();
    
    output
}

fn run_interactive_mode(config: IOTopConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting interactive pager I/O monitoring...");
    println!("Press 'q' or Ctrl+C to quit");
    
    let mut iteration = 0;
    loop {
        let data_list = get_all_iotop_data()?;
        let display_output = format_iotop_display(&data_list);
        
        if !config.batch_mode {
            print!("{}", display_output);
            io::stdout().flush()?;
        } else {
            println!("--- Iteration {} ---", iteration + 1);
            println!("{}", display_output);
        }
        
        iteration += 1;
        
        if let Some(max_count) = config.count {
            if iteration >= max_count {
                break;
            }
        }
        
        thread::sleep(Duration::from_millis(config.refresh_rate));
    }
    
    Ok(())
}

fn run_batch_mode(config: IOTopConfig) -> Result<(), Box<dyn std::error::Error>> {
    let iterations = config.count.unwrap_or(10);
    
    for i in 0..iterations {
        let data_list = get_all_iotop_data()?;
        let display_output = format_iotop_display(&data_list);
        
        println!("=== Batch {} of {} ===", i + 1, iterations);
        println!("{}", display_output);
        println!();
        
        if i < iterations - 1 {
            thread::sleep(Duration::from_millis(config.refresh_rate));
        }
    }
    
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = parse_args();
   
    if config.batch_mode {
        run_batch_mode(config)
    } else {
        run_interactive_mode(config)
    }
}

