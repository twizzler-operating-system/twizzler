use std::{
    env,
    thread,
    time::{Duration, Instant},
    ptr,
};

use twizzler::{
    object::{Object, RawObject, MapFlags},
};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE, Protections},
    syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, sys_object_create},
};

const PAGE_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug)]
enum TestMode {
    Sequential,    // Write pages sequentially
    Random,        // Random access pattern
    Pressure,      // Memory pressure test
    Batch,         // Batch allocation test
}

impl TestMode {
    fn from_str(s: &str) -> Self {
        match s {
            "sequential" => TestMode::Sequential,
            "random" => TestMode::Random,
            "pressure" => TestMode::Pressure,
            "batch" => TestMode::Batch,
            _ => TestMode::Sequential,
        }
    }
}

struct TestConfig {
    count: usize,
    delay_ms: u64,
    verbose: bool,
    persist: bool,
    mode: TestMode,
    pages_per_object: usize,
    batch_size: usize,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            count: 50,
            delay_ms: 100,
            verbose: false,
            persist: false,
            mode: TestMode::Sequential,
            pages_per_object: 100,
            batch_size: 10,
        }
    }
}

fn allocate_object_pages(persist: bool) -> std::result::Result<Object<()>, Box<dyn std::error::Error>> {
    let lifetime = if persist {
        LifetimeType::Persistent
    } else {
        LifetimeType::Volatile
    };

    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            lifetime,
            None,
            ObjectCreateFlags::empty(),
            Protections::READ | Protections::WRITE,
        ),
        &[],
        &[],
    )?;

    let flags = if persist {
        MapFlags::READ | MapFlags::WRITE | MapFlags::PERSIST
    } else {
        MapFlags::READ | MapFlags::WRITE
    };

    Object::map(id, flags).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

fn write_sequential_pages(obj: &Object<()>, pages_to_write: usize, verbose: bool) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let usable_size = MAX_SIZE - NULLPAGE_SIZE;
    let max_pages = usable_size / PAGE_SIZE;
    let actual_pages = pages_to_write.min(max_pages);

    for page in 0..actual_pages {
        let offset = NULLPAGE_SIZE + (page * PAGE_SIZE);
        
        let ptr = obj.lea_mut(offset, PAGE_SIZE).ok_or("Failed to get page pointer")?;
        
        unsafe {
            // Write a pattern that includes page number and some data
            let page_data = page as u32;
            ptr::write(ptr as *mut u32, page_data);
            ptr::write(ptr.add(PAGE_SIZE / 2) as *mut u32, page_data ^ 0xAAAAAAAA);
            ptr::write(ptr.add(PAGE_SIZE - 4) as *mut u32, page_data ^ 0x55555555);
        }

        if verbose && page % 100 == 0 {
            println!("    Sequential write to page {}/{}", page + 1, actual_pages);
        }
    }

    Ok(())
}

fn write_random_pages(obj: &Object<()>, pages_to_write: usize, verbose: bool) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let usable_size = MAX_SIZE - NULLPAGE_SIZE;
    let max_pages = usable_size / PAGE_SIZE;
    let actual_pages = pages_to_write.min(max_pages);
    
    // Simple PRNG for deterministic "random" access
    let mut seed = 12345u32;
    
    for i in 0..actual_pages {
        // Simple linear congruential generator
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        let page = (seed as usize) % actual_pages;
        
        let offset = NULLPAGE_SIZE + (page * PAGE_SIZE);
        
        let ptr = obj.lea_mut(offset, PAGE_SIZE).ok_or("Failed to get page pointer")?;
        
        unsafe {
            // Read-modify-write pattern
            let existing = ptr::read_volatile(ptr as *const u32);
            let new_value = existing.wrapping_add(i as u32);
            ptr::write_volatile(ptr as *mut u32, new_value);
            
            // Also write to middle and end
            ptr::write_volatile(ptr.add(PAGE_SIZE / 2) as *mut u32, new_value);
            ptr::write_volatile(ptr.add(PAGE_SIZE - 4) as *mut u32, new_value);
        }

        if verbose && i % 100 == 0 {
            println!("    Random access to page {} (iteration {})", page, i + 1);
        }
    }

    Ok(())
}

fn test_sequential(config: &TestConfig) -> std::result::Result<Vec<Object<()>>, Box<dyn std::error::Error>> {
    println!("=== Sequential Access Test ===");
    let mut objects = Vec::new();
    let start_time = Instant::now();

    for i in 0..config.count {
        let iter_start = Instant::now();

        if config.verbose || i % 10 == 0 {
            println!("Iteration {}/{}: Allocating object with {} pages...", 
                     i + 1, config.count, config.pages_per_object);
        }

        match allocate_object_pages(config.persist) {
            Ok(obj) => {
                if let Err(e) = write_sequential_pages(&obj, config.pages_per_object, config.verbose) {
                    eprintln!("Failed to write to pages in iteration {}: {}", i + 1, e);
                    continue;
                }
                
                objects.push(obj);
                
                if config.verbose {
                    println!("  Completed in {:?}", iter_start.elapsed());
                }
            }
            Err(e) => {
                eprintln!("Failed to allocate object in iteration {}: {}", i + 1, e);
            }
        }

        if config.delay_ms > 0 {
            thread::sleep(Duration::from_millis(config.delay_ms));
        }
    }

    println!("Sequential test completed in {:?}", start_time.elapsed());
    Ok(objects)
}

fn test_random_access(config: &TestConfig) -> std::result::Result<Vec<Object<()>>, Box<dyn std::error::Error>> {
    println!("=== Random Access Test ===");
    let mut objects = Vec::new();
    let start_time = Instant::now();

    for i in 0..config.count {
        let iter_start = Instant::now();

        if config.verbose || i % 10 == 0 {
            println!("Iteration {}/{}: Random access test...", i + 1, config.count);
        }

        match allocate_object_pages(config.persist) {
            Ok(obj) => {
                if let Err(e) = write_random_pages(&obj, config.pages_per_object, config.verbose) {
                    eprintln!("Failed random access in iteration {}: {}", i + 1, e);
                    continue;
                }
                
                objects.push(obj);
                
                if config.verbose {
                    println!("  Random access completed in {:?}", iter_start.elapsed());
                }
            }
            Err(e) => {
                eprintln!("Failed to allocate object in iteration {}: {}", i + 1, e);
            }
        }

        if config.delay_ms > 0 {
            thread::sleep(Duration::from_millis(config.delay_ms));
        }
    }

    println!("Random access test completed in {:?}", start_time.elapsed());
    Ok(objects)
}

fn test_memory_pressure(config: &TestConfig) -> std::result::Result<Vec<Object<()>>, Box<dyn std::error::Error>> {
    println!("=== Memory Pressure Test ===");
    let mut objects = Vec::new();
    let start_time = Instant::now();
    let mut total_pages = 0;

    // Keep allocating until we hit limits or reach count
    for i in 0..config.count {
        let pages_this_round = config.pages_per_object + (i * 10); // Increasing pressure
        
        if config.verbose || i % 5 == 0 {
            println!("Pressure iteration {}/{}: {} pages (total: {})", 
                     i + 1, config.count, pages_this_round, total_pages);
        }

        match allocate_object_pages(config.persist) {
            Ok(obj) => {
                match write_sequential_pages(&obj, pages_this_round, false) {
                    Ok(_) => {
                        objects.push(obj);
                        total_pages += pages_this_round;
                        
                        if config.verbose {
                            println!("  Memory pressure: {} MB allocated", 
                                     (total_pages * PAGE_SIZE) / (1024 * 1024));
                        }
                    }
                    Err(e) => {
                        eprintln!("Memory pressure hit at {} pages: {}", total_pages, e);
                        break;
                    }
                }
            }
            Err(e) => {
                println!("Memory allocation failed at {} total pages: {}", total_pages, e);
                break;
            }
        }

        if config.delay_ms > 0 {
            thread::sleep(Duration::from_millis(config.delay_ms));
        }
    }

    println!("Memory pressure test completed in {:?}", start_time.elapsed());
    println!("Peak memory usage: ~{} MB across {} objects", 
             (total_pages * PAGE_SIZE) / (1024 * 1024), objects.len());
    Ok(objects)
}

fn test_batch_allocation(config: &TestConfig) -> std::result::Result<Vec<Object<()>>, Box<dyn std::error::Error>> {
    println!("=== Batch Allocation Test ===");
    let mut all_objects = Vec::new();
    let start_time = Instant::now();

    let batches = (config.count + config.batch_size - 1) / config.batch_size;
    
    for batch in 0..batches {
        let batch_start = Instant::now();
        let mut batch_objects = Vec::new();
        
        let batch_count = config.batch_size.min(config.count - batch * config.batch_size);
        
        println!("Batch {}/{}: Allocating {} objects...", 
                 batch + 1, batches, batch_count);

        // Allocate batch quickly
        for i in 0..batch_count {
            match allocate_object_pages(config.persist) {
                Ok(obj) => {
                    batch_objects.push(obj);
                }
                Err(e) => {
                    eprintln!("Batch allocation failed at item {}: {}", i + 1, e);
                }
            }
        }

        // Now write to all objects in batch
        for (i, obj) in batch_objects.iter().enumerate() {
            if let Err(e) = write_sequential_pages(obj, config.pages_per_object, false) {
                eprintln!("Failed to initialize batch object {}: {}", i, e);
            }
        }

        let batch_duration = batch_start.elapsed();
        println!("  Batch {} completed in {:?} ({} objects, {:.2} obj/sec)", 
                 batch + 1, 
                 batch_duration,
                 batch_objects.len(),
                 batch_objects.len() as f64 / batch_duration.as_secs_f64());

        all_objects.extend(batch_objects);

        if config.delay_ms > 0 {
            thread::sleep(Duration::from_millis(config.delay_ms));
        }
    }

    println!("Batch allocation test completed in {:?}", start_time.elapsed());
    Ok(all_objects)
}

fn parse_args() -> TestConfig {
    let args: Vec<String> = env::args().collect();
    let mut config = TestConfig::default();

    for arg in &args[1..] {
        match arg.as_str() {
            "-v" | "--verbose" => config.verbose = true,
            "--persist" => config.persist = true,
            _ if arg.starts_with("--count=") => {
                config.count = arg.split('=').nth(1).unwrap_or("50").parse().unwrap_or(50);
            }
            _ if arg.starts_with("--delay=") => {
                config.delay_ms = arg.split('=').nth(1).unwrap_or("100").parse().unwrap_or(100);
            }
            _ if arg.starts_with("--pages=") => {
                config.pages_per_object = arg.split('=').nth(1).unwrap_or("100").parse().unwrap_or(100);
            }
            _ if arg.starts_with("--batch=") => {
                config.batch_size = arg.split('=').nth(1).unwrap_or("10").parse().unwrap_or(10);
            }
            _ if arg.starts_with("--mode=") => {
                let mode_str = arg.split('=').nth(1).unwrap_or("sequential");
                config.mode = TestMode::from_str(mode_str);
            }
            _ => {}
        }
    }

    config
}

fn print_usage() {
    println!("Pager Stress Test - Test the Twizzler pager server");
    println!();
    println!("Usage: pager_stress_test [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --count=N       Number of iterations (default: 50)");
    println!("  --delay=N       Delay between iterations in ms (default: 100)");
    println!("  --pages=N       Pages per object (default: 100)");
    println!("  --batch=N       Batch size for batch mode (default: 10)");
    println!("  --mode=MODE     Test mode: sequential, random, pressure, batch (default: sequential)");
    println!("  --persist       Use persistent objects instead of volatile");
    println!("  -v, --verbose   Verbose output");
    println!();
    println!("Examples:");
    println!("  pager_stress_test --mode=pressure --count=100");
    println!("  pager_stress_test --mode=random --pages=500 --verbose");
    println!("  pager_stress_test --mode=batch --batch=20 --count=200");
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let config = parse_args();

    println!("=== Pager Stress Test ===");
    println!("Mode: {:?}", config.mode);
    println!("Count: {}, Pages/object: {}, Delay: {}ms", 
             config.count, config.pages_per_object, config.delay_ms);
    println!("Persist: {}, Verbose: {}", config.persist, config.verbose);
    println!();

    if config.count == 0 {
        print_usage();
        return Ok(());
    }

    let start_time = Instant::now();
    
    let objects = match config.mode {
        TestMode::Sequential => test_sequential(&config)?,
        TestMode::Random => test_random_access(&config)?,
        TestMode::Pressure => test_memory_pressure(&config)?,
        TestMode::Batch => test_batch_allocation(&config)?,
    };

    let total_duration = start_time.elapsed();
    let total_pages = objects.len() * config.pages_per_object;
    let total_memory_mb = (total_pages * PAGE_SIZE) / (1024 * 1024);
    
    println!();
    println!("=== Test Results ===");
    println!("Mode: {:?}", config.mode);
    println!("Objects allocated: {}", objects.len());
    println!("Total pages: {}", total_pages);
    println!("Total memory: {} MB", total_memory_mb);
    println!("Total time: {:?}", total_duration);
    println!("Average: {:.2} objects/sec", objects.len() as f64 / total_duration.as_secs_f64());
    
    if objects.len() > 0 {
        println!("Average per object: {:.2} ms", total_duration.as_millis() as f64 / objects.len() as f64);
    }
    
    println!();
    println!("Test completed successfully! Objects remain allocated.");
    println!("This will test pager memory management and page-out behavior.");

    Ok(())
}
