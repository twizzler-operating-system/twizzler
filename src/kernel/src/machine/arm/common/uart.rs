/// A PL011 UART Driver Interface
/// 
/// Full specification found here: https://developer.arm.com/documentation/ddi0183/latest

// #[derive(Copy, Clone)]
pub struct PL011 {
    /// base address of the MMIO UART
    base: usize,
}

/// Register offsets from UART MMIO base address
#[repr(u32)]
#[allow(non_camel_case_types)]
enum Registers {
    UARTDR = 0x000,
    UARTFR = 0x018,
    UARTIBRD = 0x024,
    UARTFBRD = 0x028,
    UARTLCR_H = 0x02C,
    UARTCR = 0x030,
    UARTIMSC = 0x038,
    UARTICR = 0x044,
    UARTDMACR = 0x048,
}

impl PL011 {
    /// Create new instance of a PL011 UART at some base address
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            base: base
        }
    }

    /// Transmit a single byte of data. Will block to send other data present before.
    pub fn tx_byte(&self, data: u8) {
        // wait for any other characters to send
        unsafe {
            loop {
                let flag_reg = self.read_reg(Registers::UARTFR);
                // bit is set if no data is present in holding register
                // or if operating in FIFO mode, FIFO is empty
                let tx_empty: bool = ((flag_reg >> 7) & 0b1) == 1;
                if tx_empty  {
                    break
                }
            }
            self.write_reg(Registers::UARTDR, data as u32);
        }
    }

    /// Recieve a single byte of data if present in a non-blocking way.
    pub fn rx_byte(&self) -> Option<u8> {
        // the rx holding register/fifo may be empty
        // check if RXFF bit in the UARTFR is set
        let flag_reg = unsafe { self.read_reg(Registers::UARTFR) };
        let rx_empty = (flag_reg >> 4) & 0b1 == 1;
        if rx_empty {
            return None
        }

        // received data byte is read by performing reads from the UARTDR Register 
        // along with the corresponding status information
        let data = unsafe { self.read_reg(Registers::UARTDR) };

        // TODO: check for rx errors
        // received data character must be read first from UARTDR
        // before reading the error status UARTRSR
        Some((data & 0xff) as u8)
    }

    /// Configure the PL011 UART with desired baud, given the clock frequency
    pub unsafe fn init(&self, baud: u32, clk: u32) {
        // program the UART: (page 26/3-16)
        // disable UART
        {
            let cr = self.read_reg(Registers::UARTCR);
            self.write_reg(Registers::UARTCR, (cr &  !0b1) as u32);
        }
        // wait for end of tx or rx of current char
        // while BUSY, !TXFE, !RXFE --> wait
        loop {
            let flag_reg = self.read_reg(Registers::UARTFR);
            let tx_busy = (flag_reg >> 3) & 0b1 == 1;
            let tx_empty = (flag_reg >> 7) & 0b1 == 1;
            let rx_empty = (flag_reg >> 4) & 0b1 == 1;

            if !tx_busy && tx_empty && rx_empty {
                break
            }
        }
        // Flush the transmit FIFO by setting the FEN bit to 0 in the 
        // Line Control Register, UARTLCR_H on page 3-12.
        let lcr = self.read_reg(Registers::UARTLCR_H);
        self.write_reg(Registers::UARTLCR_H, (lcr & !(0b1 << 4)) as u32);

        // calculate the divisors to program UARTIBRD/UARTFBRD
        // according to 3.3.6:
        //
        // Baud rate divisor = (UART clock frequency / (16 * Baud rate))
        // Baud rate divisor is composed of an integer value and a fractional value
        //
        // brd = int + frac = freq / (16 * baud)
        // ~= int + f / 64 = freq / (16 * baud)
        // 64 * brd = 4 * clk / baud
        // int = (64 * brd - f) / 64 (integer division)
        // since f / 64 < 1, int = (64 * brd) / 64
        // f ~= to 6 remaining bits after division
        let brd_scaled: u32 = 4 * clk / baud; // brd * 64
        let int: u32 = brd_scaled >> 6;
        let frac: u32 = brd_scaled & 0x3f;
        
        // configure channel format: parity, word size, etc.
        // no parity, 1 stop bit, 8 data bits
        // brk 0, parity 0, eps 0, fifo enable 0, 2 stop bits 0, wlen 0b11 (8 bits), stick parity 1
        let lcr = self.read_reg(Registers::UARTLCR_H) | 0b11 << 5;

        // write out changes
        self.write_reg(Registers::UARTIBRD, int);
        self.write_reg(Registers::UARTFBRD, frac);
        self.write_reg(Registers::UARTLCR_H, lcr);
        
        // disable interrupts
        self.write_reg(Registers::UARTIMSC, 0u32);

        // disable dma
        self.write_reg(Registers::UARTDMACR, 0u32);

        // reprogram uartcr register
        {
            let cr = self.read_reg(Registers::UARTCR);
            // enable rx and tx
            self.write_reg(Registers::UARTCR, (cr | (0b1 << 8) | (0b1 << 9)) as u32);
        }
        // enable uart
        {
            let cr = self.read_reg(Registers::UARTCR);
            self.write_reg(Registers::UARTCR, (cr | 0b1) as u32);
        }
    }

    /// Enable interrupts for the RX side if the UART.
    pub unsafe fn enable_rx_interrupt(&self) {
        // program the UART: (page 62/3-16)
        // disable UART
        {
            let cr = self.read_reg(Registers::UARTCR);
            self.write_reg(Registers::UARTCR, (cr &  !0b1) as u32);
        }
        // wait for end of tx or rx of current char
        // while BUSY, !TXFE, !RXFE --> wait
        loop {
            let flag_reg = self.read_reg(Registers::UARTFR);
            let tx_busy = (flag_reg >> 3) & 0b1 == 1;
            let tx_empty = (flag_reg >> 7) & 0b1 == 1;
            let rx_empty = (flag_reg >> 4) & 0b1 == 1;

            if !tx_busy && tx_empty && rx_empty {
                break
            }
        }
        // Flush the transmit FIFO by setting the FEN bit to 0 in the
        // Line Control Register, UARTLCR_H on page 3-12.
        let lcr = self.read_reg(Registers::UARTLCR_H);
        self.write_reg(Registers::UARTLCR_H, (lcr & !(0b1 << 4)) as u32);

        // Enable interrupts for the RX side.
        // See RXIM: bit 4 of UARTIMSC from table 3-14, pg 3-18
        // - On a write of 1, the mask of the UARTRXINTR interrupt is set.
        // - A write of 0 clears the mask.
        // From 2.8, pg 2-22: "Setting the mask bit HIGH enables the interrupt."
        self.write_reg(Registers::UARTIMSC, (0b1 << 4) as u32);

        // enable uart
        {
            let cr = self.read_reg(Registers::UARTCR);
            self.write_reg(Registers::UARTCR, (cr | 0b1) as u32);
        }
    }

    pub fn clear_rx_interrupt(&self) {
        // The UARTICR Register is the interrupt clear register and is write-only. 
        // On a write of 1, the corresponding interrupt is cleared. 
        // A write of 0 has no effect. Table 3-17 lists the register bit assignments.
        //
        // We must write to Receive interrupt clear (RXIC) which is bit 4. This
        // Clears the UARTRXINTR interrupt.
        unsafe {
            self.write_reg(Registers::UARTICR, (0b1 << 4) as u32);
        }
    }

    /// Write a value to a single register. Registers are 32-bits wide.
    unsafe fn write_reg(&self, register: Registers, value: u32) {
        let reg = (self.base + register as usize) as *mut u32;
        reg.write_volatile(value as u32)
    }

    /// Read a value to a single register. Registers are 32-bits wide.
    unsafe fn read_reg(&self, register: Registers) -> u32 {
        let reg = (self.base + register as usize) as *const u32;
        reg.read_volatile()
    }
}

mod error {
    /// Errors that occur when recieving data according to Table 3-3 UARTRSR/UARTECR Register.
    /// The received data character must be read first from the Data Register, UARTDR
    /// before reading the error status associated with that data character.
    enum Error {
        /// Data is recieved when FIFO is already full. The data in the FIFO remains valid. 
        /// The shift register is overwritten. The CPU must read the data to empty the FIFO.
        OverrunError,
        /// The received data input was held LOW for longer than a full-word transmission time.
        /// The error is cleared after a write to UARTECR.
        BreakError,
        /// Parity does not match the parity of EPS/SPS bits in the Line Control Register, UARTLCR_H.
        /// The error is cleared after a write to UARTECR.
        ParityError,
        /// The received character did not have a valid stop bit (a valid stop bit is 1).
        /// The error is cleared after a write to UARTECR.
        FramingError,
    }
}
