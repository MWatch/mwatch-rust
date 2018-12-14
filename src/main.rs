#![deny(warnings)]

#![no_std]
#![no_main]

#[macro_use]
extern crate cortex_m;
extern crate cortex_m_rtfm as rtfm;
extern crate cortex_m_semihosting as sh;
extern crate panic_semihosting;
extern crate heapless;
extern crate ssd1351;
extern crate embedded_graphics;
extern crate stm32l4xx_hal as hal;
extern crate max17048;
extern crate hm11;

#[macro_use(entry, exception)]
extern crate cortex_m_rt as rt;

use embedded_graphics::Drawing;
use rt::ExceptionFrame;
use hal::dma::{dma1, CircBuffer, Event};
use hal::prelude::*;
use hal::serial::{Serial, Event as SerialEvent};
use hal::timer::{Timer, Event as TimerEvent};
use hal::delay::Delay;
use hal::spi::Spi;
use hal::i2c::I2c;
use hal::rtc::Rtc;
use hal::datetime::Date;
use hal::tsc::{Tsc, Event as TscEvent, Config as TscConfig, ClockPrescaler as TscClockPrescaler};
use hal::stm32l4::stm32l4x2;
use heapless::spsc::Queue;
use heapless::String;
use heapless::consts::*;
use rtfm::{app, Threshold, Resource};

use core::fmt::Write;

use ssd1351::builder::Builder;
use ssd1351::mode::{GraphicsMode};
use ssd1351::prelude::*;
use ssd1351::properties::DisplayRotation;

use embedded_graphics::prelude::*;
use embedded_graphics::fonts::Font12x16;
use embedded_graphics::fonts::Font6x12;
use embedded_graphics::image::Image16BPP;

use max17048::Max17048;
use hm11::Hm11;
use hm11::command::Command;

/* Our includes */
mod msgmgr;

use msgmgr::MSG_SIZE;
use msgmgr::MSG_COUNT;
use msgmgr::Message;
use msgmgr::MessageManager;

/// Type Alias to use in resource definitions
type Ssd1351 = ssd1351::mode::GraphicsMode<ssd1351::interface::SpiInterface<hal::spi::Spi<hal::stm32l4::stm32l4x2::SPI1, (hal::gpio::gpioa::PA5<hal::gpio::Alternate<hal::gpio::AF5, hal::gpio::Input<hal::gpio::Floating>>>, hal::gpio::gpioa::PA6<hal::gpio::Alternate<hal::gpio::AF5, hal::gpio::Input<hal::gpio::Floating>>>, hal::gpio::gpioa::PA7<hal::gpio::Alternate<hal::gpio::AF5, hal::gpio::Input<hal::gpio::Floating>>>)>, hal::gpio::gpiob::PB1<hal::gpio::Output<hal::gpio::PushPull>>>>;

#[entry]
fn main() -> ! {
    rtfm_main() // this is generated by the app! macro - only in my fork of rtfm
}

#[exception]
fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("{:#?}", ef);
}

const DMA_HAL_SIZE: usize = 64;

app! {
    device: stm32l4x2,

    resources: {
        static DMA_BUFFER: [[u8; crate::DMA_HAL_SIZE]; 2] = [[0; crate::DMA_HAL_SIZE]; 2];
        static CB: CircBuffer<&'static mut [[u8; DMA_HAL_SIZE]; 2], dma1::C6>;
        static MSG_PAYLOADS: [[u8; crate::MSG_SIZE]; crate::MSG_COUNT] = [[0; crate::MSG_SIZE]; crate::MSG_COUNT];
        static MMGR: MessageManager;
        static RB: heapless::spsc::Queue<u8, heapless::consts::U256> = heapless::spsc::Queue::new();
        static USART2_RX: hal::serial::Rx<hal::stm32l4::stm32l4x2::USART2>;
        static DISPLAY: Ssd1351;
        static RTC: hal::rtc::Rtc;
        static TOUCH: hal::tsc::Tsc<hal::gpio::gpiob::PB4<hal::gpio::Alternate<hal::gpio::AF9, hal::gpio::Output<hal::gpio::OpenDrain>>>>;
        static OK_BUTTON: hal::gpio::gpiob::PB5<hal::gpio::Alternate<hal::gpio::AF9, hal::gpio::Output<hal::gpio::PushPull>>>;
        static CHRG: hal::gpio::gpioa::PA12<hal::gpio::Input<hal::gpio::PullUp>>;
        static STDBY: hal::gpio::gpioa::PA11<hal::gpio::Input<hal::gpio::PullUp>>;
        static BT_CONN: hal::gpio::gpioa::PA8<hal::gpio::Input<hal::gpio::Floating>>;
        static BMS: max17048::Max17048<hal::i2c::I2c<hal::stm32::I2C1, (hal::gpio::gpioa::PA9<hal::gpio::Alternate<hal::gpio::AF4, hal::gpio::Output<hal::gpio::OpenDrain>>>, hal::gpio::gpioa::PA10<hal::gpio::Alternate<hal::gpio::AF4, hal::gpio::Output<hal::gpio::OpenDrain>>>)>>;
        static TOUCH_THRESHOLD: u16;
        static TOUCHED: bool = false;
        static WAS_TOUCHED: bool = false;
        static STATE: u8 = 0;
    },

    init: {
        resources: [DMA_BUFFER, MSG_PAYLOADS, RB],
    },

    tasks: {
        TIM2: {
            path: sys_tick,
            resources: [MMGR, DISPLAY, RTC, TOUCHED, WAS_TOUCHED, STATE, BMS, STDBY, CHRG, BT_CONN],
        },
        DMA1_CH6: { /* DMA channel for Usart1 RX */
            priority: 2,
            path: rx_full,
            resources: [CB, MMGR],
        },
        USART2: { /* Global usart1 it, uses for idle line detection */
            priority: 2,
            path: rx_idle,
            resources: [CB, MMGR, USART2_RX],
        },
        TSC: {
            priority: 2, /* Input should always preempt other tasks */
            path: touch,
            resources: [OK_BUTTON, TOUCH, TOUCH_THRESHOLD, TOUCHED]
        },
        EXTI15_10: {
            priority: 2,
            path: alrt,
        }
    }
}

fn init(p: init::Peripherals, r: init::Resources) -> init::LateResources {

    let mut flash = p.device.FLASH.constrain();
    let mut rcc = p.device.RCC.constrain();
    let clocks = rcc.cfgr.sysclk(32.mhz()).pclk1(32.mhz()).pclk2(32.mhz()).freeze(&mut flash.acr);
    // let clocks = rcc.cfgr.freeze(&mut flash.acr);
    
    let mut gpioa = p.device.GPIOA.split(&mut rcc.ahb2);
    let mut gpiob = p.device.GPIOB.split(&mut rcc.ahb2);
    let mut channels = p.device.DMA1.split(&mut rcc.ahb1);
    

    let mut pwr = p.device.PWR.constrain(&mut rcc.apb1r1);
    let rtc = Rtc::rtc(p.device.RTC, &mut rcc.apb1r1, &mut rcc.bdcr, &mut pwr.cr1);

    let date = Date::new(1.day(), 07.date(), 10.month(), 2018.year());
    rtc.set_date(&date);

    /* Ssd1351 Display */
    let mut delay = Delay::new(p.core.SYST, clocks);
    let mut rst = gpiob
        .pb0
        .into_push_pull_output(&mut gpiob.moder, &mut gpiob.otyper);

    let dc = gpiob
        .pb1
        .into_push_pull_output(&mut gpiob.moder, &mut gpiob.otyper);

    let sck = gpioa.pa5.into_af5(&mut gpioa.moder, &mut gpioa.afrl);
    let miso = gpioa.pa6.into_af5(&mut gpioa.moder, &mut gpioa.afrl);
    let mosi = gpioa.pa7.into_af5(&mut gpioa.moder, &mut gpioa.afrl);

    let spi = Spi::spi1(
        p.device.SPI1,
        (sck, miso, mosi),
        SSD1351_SPI_MODE,
        16.mhz(),
        clocks,
        &mut rcc.apb2,
    );

    let mut display: GraphicsMode<_> = Builder::new().connect_spi(spi, dc).into();
    display.reset(&mut rst, &mut delay);
    display.init().unwrap();
    display.set_rotation(DisplayRotation::Rotate0).unwrap();

    /* Serial with DMA */
    // usart 1
    // let tx = gpioa.pa9.into_af7(&mut gpioa.moder, &mut gpioa.afrh);
    // let rx = gpioa.pa10.into_af7(&mut gpioa.moder, &mut gpioa.afrh);
    // let mut serial = Serial::usart1(p.device.USART1, (tx, rx), 9_600.bps(), clocks, &mut rcc.apb2);
    
    let tx = gpioa.pa2.into_af7(&mut gpioa.moder, &mut gpioa.afrl);
    let rx = gpioa.pa3.into_af7(&mut gpioa.moder, &mut gpioa.afrl);
    
    let mut serial = Serial::usart2(p.device.USART2, (tx, rx), 115200.bps(), clocks, &mut rcc.apb1r1);
    serial.listen(SerialEvent::Idle); // Listen to Idle Line detection, IT not enable until after init is complete
    let (tx, rx) = serial.split();

    delay.delay_ms(100_u8); // allow module to boot
    let mut hm11 = Hm11::new(tx, rx); // tx, rx into hm11 for configuration
    hm11.send_with_delay(Command::Test, &mut delay).unwrap();
    hm11.send_with_delay(Command::SetName("MWatch"), &mut delay).unwrap();
    hm11.send_with_delay(Command::SystemLedMode(true), &mut delay).unwrap();
    hm11.send_with_delay(Command::Reset, &mut delay).unwrap();
    delay.delay_ms(100_u8); // allow module to reset
    hm11.send_with_delay(Command::Test, &mut delay).unwrap(); // has the module come back up?
    let (_, rx) = hm11.release();

    
    channels.6.listen(Event::HalfTransfer);
    channels.6.listen(Event::TransferComplete);

    /* Touch sense controller */
    let sample_pin = gpiob.pb4.into_touch_sample(&mut gpiob.moder, &mut gpiob.otyper, &mut gpiob.afrl);
    let mut ok_button = gpiob.pb5.into_touch_channel(&mut gpiob.moder, &mut gpiob.otyper, &mut gpiob.afrl);
    let tsc_config = TscConfig {
        clock_prescale: Some(TscClockPrescaler::HclkDiv32),
        max_count_error: None
    };
    let mut tsc = Tsc::tsc(p.device.TSC, sample_pin, &mut rcc.ahb1, Some(tsc_config));
    
    // Acquire for rough estimate of capacitance
    const NUM_SAMPLES: u16 = 25;
    let mut baseline = 0;
    for _ in 0..NUM_SAMPLES {
        baseline += tsc.acquire(&mut ok_button).unwrap();
    }
    let threshold = ((baseline / NUM_SAMPLES) / 100) * 90;

    /* T4056 input pins */
    let stdby = gpioa.pa11.into_pull_up_input(&mut gpioa.moder, &mut gpioa.pupdr);
    let chrg = gpioa.pa12.into_pull_up_input(&mut gpioa.moder, &mut gpioa.pupdr);
    let bt_conn = gpioa.pa8.into_floating_input(&mut gpioa.moder, &mut gpioa.pupdr);

    /* Fuel Guage */
    let mut scl = gpioa.pa9.into_open_drain_output(&mut gpioa.moder, &mut gpioa.otyper);
    scl.internal_pull_up(&mut gpioa.pupdr, true);
    let scl = scl.into_af4(&mut gpioa.moder, &mut gpioa.afrh);

    let mut sda = gpioa.pa10.into_open_drain_output(&mut gpioa.moder, &mut gpioa.otyper);
    sda.internal_pull_up(&mut gpioa.pupdr, true);
    let sda = sda.into_af4(&mut gpioa.moder, &mut gpioa.afrh);
    
    let i2c = I2c::i2c1(p.device.I2C1, (scl, sda), 100.khz(), clocks, &mut rcc.apb1r1);

    let max17048 = Max17048::new(i2c);

    p.device.EXTI.imr1.write(|w| w.mr15().set_bit()); // unmask the interrupt (EXTI15)
    let mut alrt = gpioa.pa15.into_open_drain_output(&mut gpioa.moder, &mut gpioa.otyper);
    alrt.internal_pull_up(&mut gpioa.pupdr, true);
    // alrt pin is PA15 - EXTI15
    p.device.EXTI.ftsr1.write(|w| w.tr15().set_bit());
    
    /* Static RB for Msg recieving */
    let rb: &'static mut Queue<u8, U256> = r.RB;
    /* Define out block of message - surely there must be a nice way to to this? */
    let msgs: [msgmgr::Message; MSG_COUNT] = [
        Message::new(r.MSG_PAYLOADS[0]),
        Message::new(r.MSG_PAYLOADS[1]),
        Message::new(r.MSG_PAYLOADS[2]),
        Message::new(r.MSG_PAYLOADS[3]),
        Message::new(r.MSG_PAYLOADS[4]),
        Message::new(r.MSG_PAYLOADS[5]),
        Message::new(r.MSG_PAYLOADS[6]),
        Message::new(r.MSG_PAYLOADS[7]),
    ];


    /* Pass messages to the Message Manager */
    let mmgr = MessageManager::new(msgs, rb);

    let mut systick = Timer::tim2(p.device.TIM2, 10.hz(), clocks, &mut rcc.apb1r1);
    systick.listen(TimerEvent::TimeOut);

    // input 'thread' poll the touch buttons - could we impl a proper hardare solution with the TSC?
    // let mut input = Timer::tim7(p.device.TIM7, 20.hz(), clocks, &mut rcc.apb1r1);
    // input.listen(TimerEvent::TimeOut);

    tsc.listen(TscEvent::EndOfAcquisition);
    // tsc.listen(TscEvent::MaxCountError); // TODO
    // we do this to kick off the tsc loop - the interrupt starts a reading everytime one completes
    rtfm::set_pending(stm32l4x2::Interrupt::TSC);
    let buffer: &'static mut [[u8; crate::DMA_HAL_SIZE]; 2] = r.DMA_BUFFER;

    init::LateResources {
        CB: rx.circ_read(channels.6, buffer),
        MMGR: mmgr,
        USART2_RX: rx,
        DISPLAY: display,
        RTC: rtc,
        TOUCH: tsc,
        OK_BUTTON: ok_button,
        TOUCH_THRESHOLD: threshold,
        BMS: max17048,
        STDBY: stdby,
        CHRG: chrg,
        BT_CONN: bt_conn
    }
}

fn idle() -> ! {
    loop {
        rtfm::wfi(); /* Wait for interrupts - sleep mode */
    }
}

fn alrt() {

}

/// Handles a full or hal full dma buffer of serial data,
/// and writes it into the MessageManager rb
fn rx_full(_t: &mut Threshold, mut r: DMA1_CH6::Resources) {
    let mut mgr = r.MMGR;
    r.CB
        .peek(|buf, _half| {
            mgr.write(buf);
        })
        .unwrap();
}

/// Handles the intermediate state where the DMA has data in it but
/// not enough to trigger a half or full dma complete
fn rx_idle(_t: &mut Threshold, mut r: USART2::Resources) {
    let mut mgr = r.MMGR;
    if r.USART2_RX.is_idle(true) {
        r.CB
            .partial_peek(|buf, _half| {
                let len = buf.len();
                if len > 0 {
                    mgr.write(buf);
                }
                Ok( (len, ()) )
            })
            .unwrap();
    }
}

fn sys_tick(t: &mut Threshold, mut r: TIM2::Resources) {
    let mut mgr = r.MMGR;
    let mut display = r.DISPLAY;
    let mut buffer: String<U256> = String::new();

    let msg_count = mgr.claim_mut(t, | m, _t| {
        m.process();
        m.msg_count()
    });
    
    let time = r.RTC.get_time();
    let _date = r.RTC.get_date();

    let current_touched = r.TOUCHED.claim(t, | val, _| *val);
    if current_touched != *r.WAS_TOUCHED {
        *r.WAS_TOUCHED = current_touched;
        if current_touched == true {
            display.clear();
            *r.STATE += 1;
            if *r.STATE > 4 {
                *r.STATE = 0;
            }
        }
    }

    match *r.STATE {
        // HOME PAGE
        0 => {
            write!(buffer, "{:02}:{:02}:{:02}", time.hours, time.minutes, time.seconds).unwrap();
            display.draw(Font12x16::render_str(buffer.as_str())
                .translate(Coord::new(10, 40))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
            buffer.clear(); // reset the buffer
            // write!(buffer, "{:02}:{:02}:{:04}", date.date, date.month, date.year).unwrap();
            write!(buffer, "BT={}", r.BT_CONN.is_high()).unwrap();
            display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(24, 60))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
            buffer.clear(); // reset the buffer
            write!(buffer, "{:02}", msg_count).unwrap();
            display.draw(Font12x16::render_str(buffer.as_str())
                .translate(Coord::new(46, 96))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
            buffer.clear(); // reset the buffer
            let soc = bodged_soc(r.BMS.soc().unwrap());
            write!(buffer, "{:02}%", soc).unwrap();
            display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(110, 12))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
            buffer.clear(); // reset the buffer
            write!(buffer, "{:03.03}v", r.BMS.vcell().unwrap()).unwrap();
            display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(0, 12))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
            buffer.clear(); // reset the buffer
            if r.CHRG.is_low() {
                write!(buffer, "CHRG").unwrap();
                display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(48, 12))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
                buffer.clear(); // reset the buffer
                if let Some(soc_per_hr) = r.BMS.charge_rate().ok() {
                    if soc_per_hr < 200.0 {
                        write!(buffer, "{:03.1}%/hr", soc_per_hr).unwrap();
                        display.draw(Font6x12::render_str(buffer.as_str())
                        .translate(Coord::new(32, 116))
                        .with_stroke(Some(0x2679_u16.into()))
                        .into_iter());
                        buffer.clear(); // reset the buffer
                    }
                }
            } else if r.STDBY.is_high() {
                write!(buffer, "STDBY").unwrap();
                display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(48, 12))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
                buffer.clear(); // reset the buffer
            } else {
                write!(buffer, "DONE").unwrap();
                display.draw(Font6x12::render_str(buffer.as_str())
                .translate(Coord::new(48, 12))
                .with_stroke(Some(0x2679_u16.into()))
                .into_iter());
                buffer.clear(); // reset the buffer
            }
        },
        // MESSAGE LIST
        1 => {
            mgr.claim_mut(t, |m, _t| {
                if msg_count > 0 {
                    for i in 0..msg_count {
                        m.peek_message(i, |msg| {
                            write!(buffer, "[{}]: ", i + 1);
                            for c in 0..msg.payload_idx {
                                buffer.push(msg.payload[c] as char).unwrap();
                            }
                            display.draw(Font6x12::render_str(buffer.as_str())
                                .translate(Coord::new(0, (i * 12) as i32 + 2))
                                .with_stroke(Some(0xF818_u16.into()))
                                .into_iter());
                            buffer.clear();
                        });
                    }
                } else {
                    display.draw(Font6x12::render_str("No messages.")
                        .translate(Coord::new(0, 12))
                        .with_stroke(Some(0xF818_u16.into()))
                        .into_iter());
                }
            });
            let stack_space = get_free_stack();
            write!(buffer, "{} bytes free",stack_space);
            display.draw(Font6x12::render_str(buffer.as_str())
            .translate(Coord::new(18, 116))
            .with_stroke(Some(0xF818_u16.into()))
            .into_iter());
            buffer.clear();
        },
        // MWATCH LOGO
        2 => {
            display.draw(Image16BPP::new(include_bytes!("../data/mwatch.raw"), 64, 64)
                .translate(Coord::new(32,32))
                .into_iter());
        },
        // UOP LOGO
        3 => {
            display.draw(Image16BPP::new(include_bytes!("../data/uop.raw"), 48, 64)
                .translate(Coord::new(32,32))
                .into_iter());
        },
        // RPR LOGO
        4 => {
            display.draw(Image16BPP::new(include_bytes!("../data/rpr.raw"), 64, 39)
                .translate(Coord::new(32,32))
                .into_iter());
        },
        _ => panic!("Unknown state")
    }
}

fn bodged_soc(raw: u16) -> u16 {
    let rawf = raw as f32;
    // let min = 0.0;
    let max = 80.0;
    let mut soc = ((rawf / max) * 100.0) as u16;
    if soc > 100 {
        soc = 100; // cap at 100
    }
    soc
}

fn get_free_stack() -> usize {
    unsafe {
        extern "C" {
            static __ebss: u32;
            static __sdata: u32;
        }
        let ebss = &__ebss as *const u32 as usize;
        let start = &__sdata as *const u32 as usize;
        let total = ebss - start;
        (64 * 1024) - total
    }
}

fn touch(_t: &mut Threshold, mut r: TSC::Resources) {
    // let reading = r.TOUCH.read_unchecked();
    let reading = r.TOUCH.read(&mut *r.OK_BUTTON).unwrap();
    let threshold = *r.TOUCH_THRESHOLD;
    if reading < threshold {
        *r.TOUCHED = true;
    } else {
        *r.TOUCHED = false;
    }
    r.TOUCH.start(&mut *r.OK_BUTTON);
}
