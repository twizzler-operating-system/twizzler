use crate::time::{ClockHardware, Ticks};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

// 1 ms = 1000000 ns
// 200 milliseconds
const SLEEP_TIME: u64 = 200 * 1_000_000; 

// resolution expressed as a unit of time
pub struct Tsc {
    resolution: FemtoSeconds
}

impl Tsc {
    pub fn new() -> Self {
        // calculate the frequency at which the TSC is running
        // in other words the resolution at which ticks occur
        let f = Tsc::get_tsc_frequency();

        logln!("[kernel::arch::tsc] tsc frequency {} (Hz), {} fs", f, 1_000_000_000_000_000 / f);
        Self {
            resolution: FemtoSeconds(1_000_000_000_000_000 / f)
        }
    }

    // returns frequency of the tsc as a u64 in Hz
    // this value might be the nominal tsc frequency, 
    // as in what we would expect in an ideal world. This
    // might need to be adjusted depending on the state
    // of the machine/crystal driving the tsc.
    fn get_tsc_frequency() -> u64 {
        // attempt to calculate frequency using cpuid
        match feature_info_frequency() {
           Ok(f) => return f,
           Err(e) => logln!("[kernel::arch::tsc] switching to pit calibration: {:?}", e),
       }

       // calculate frequency using pit timer
       return pit_frequency_estimation();
    }
}

impl ClockHardware for Tsc {
    fn read(&self) -> Ticks {
        Ticks{
            value: unsafe { x86::time::rdtsc() }, // raw timer ticks (unitless)
            rate: self.resolution
        }
    }
    fn info(&self) -> ClockInfo {
        ClockInfo::ZERO
    }
}

#[derive(Debug)]
#[repr(u32)]
enum TscError {
    LeafNotupported(u32),
    CpuFeatureNotSupported,
}

fn feature_info_frequency() -> Result<u64, TscError> {
    let cpuid = x86::cpuid::CpuId::new();
        
    // According to the Intel x86_64 manual, 
    // Volume 3, Section 18.7.3: 
    // TSC Freq = (ecx * ebx) / eax (for leaf 0x15)
    let tsc = match cpuid.get_tsc_info() {
        Some(x) => x,
        // we are probably on some old processor
        None => return Err(TscError::LeafNotupported(0x15))
        // unimplemented!("TSC leaf 0x15 not supported")
    };

    if let Some(freq) = tsc.tsc_frequency() {
        return Ok(freq)
    }

    // most likely this means that 0x15.eax was 0
    // in other words, the core crystal clock frequency
    // was not reported. We might still be able to use the
    // TSC/crystal core ratio if enumerated
    if tsc.numerator() != 0 && tsc.denominator() != 0 {
        // We refer to Table 18-85 here to provide us the 
        // core crystal clock frequency.
        let feature = match cpuid.get_feature_info() {
            Some(x) => x,
            None => unimplemented!()
        };

        // for certian Xeon processors, we can use a core
        // crystal clock frequency of 25 MHz.
        if feature.family_id() == 0x06 && feature.model_id() == 0x55 {
            let crystal: u32 = 25 * 1_000_000; // in Hz
            let freq: u64 = ((crystal as u64 * 
                tsc.numerator() as u64) / tsc.denominator() as u64)
                .into();
            return Ok(freq)
        }
    }
    
    // cpuid leaf ranges 0x40000000-0x4FFFFFFF not used by the cpu.
    // They can be used by software such as hypervisors to return
    // information to the guest. Maybe the hypervisor can tell 
    // us what the TSC frequency is. Frequency returned in kHz
    if let Some(hyperv) = cpuid.get_hypervisor_info() {
        match hyperv.tsc_frequency() {
            Some(freq) => if freq > 0 { return Ok(1_000 * freq as u64) },
            None => return Err(TscError::LeafNotupported(0x40000010))
        }
    }

    // if we reached this point, this might be on some unsupported
    // cpu which we do not take into account that does not report
    // the tsc ratio via leaf 0x15. Or the hypervisor does not support
    // returning timing info
    Err(TscError::CpuFeatureNotSupported)
    // unimplemented!("unsupported cpu TSC frequency calculation");
}

fn pit_frequency_estimation() -> u64 {
    let start = unsafe { x86::time::rdtsc() };
    super::pit::wait_ns(SLEEP_TIME);
    let end = unsafe { x86::time::rdtscp() };
    // nano is 1e-9, multiply by 1e9 to get Hz
    return ((end - start) * 1_000_000_000) / SLEEP_TIME;
}