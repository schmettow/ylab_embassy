#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
use {defmt_rtt as _, panic_probe as _};

/// # YLab Edge
/// 
/// __YLab Edge__ is a sensor recording firmware for the Cytron Maker Pi Pico
/// board.
/// 
/// ## Dependencies
/// 
/// YLab Edge makes use of the Embassy framework, in particular:
/// 
/// + multi-core
/// For running multicore, we need Executor (not just spawner) 
/// and deformat macros (!unwrap)
use embassy_executor::Executor;
use embassy_rp::multicore::{spawn_core1, Stack};
use defmt::*;

/// The following code initializes the second stack, plus 
/// two heaps
static mut CORE1_STACK: Stack<4096> = Stack::new();
use static_cell::StaticCell;
static EXECUTOR0: StaticCell<Executor> = StaticCell::new();
static EXECUTOR1: StaticCell<Executor> = StaticCell::new();

/// +  multi-threading with async
// use embassy_executor::Spawner;
/// + timing using Embassy time 
// use embassy_time::{Duration, Ticker};
/// + peripherals
use embassy_rp::gpio::Pin;
/// + thread-safe types
//use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
//use embassy_sync::signal::Signal;

// use ylab::yuio::disp::FourLines;
/// Furthermore, YLab Edge brings its own high-level modules
/// for rapidly developing interactive data collection devices.
/// 
/// + YUI input (LED) and output (button)
use ylab::yuio::led as yled;
use ylab::yuio::disp as ydsp;
use ylab::yuii::btn as ybtn;
/// + four built-in ADC sensors
use ylab::ysns::adc as yadc;
/// + four ADCs on a ADS1115;
use ylab::ysns::ads1115 as yads;

/// + data transport/storage
//use ylab::ysns::yxz as yxz;
//use ylab::ysns::ads1115 as yads;
use ylab::ytfk::bsu as ybsu;

/// ## Storage task
/// 
/// The storage task currently connects one sensor two the output channel. 
/// It lives as its own task, so one can connect multiple sources to multiple sinks,
/// e.g. storing part of the data on SD card and sending the rest up BSU.

use core::sync::atomic::Ordering;
use core::sync::atomic::AtomicBool;
static RECORD: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
async fn storage_task() { 
    loop {
        //let record = RECORD.lock(|f| f.clone().into_inner());
        //let record = false;
        // if record {
        if RECORD.load(Ordering::Relaxed){
            let result 
                = yads::RESULT.wait().await;
            yled::LED.signal(yled::State::Interrupt);
            log::info!("{},{},{},{},{}", 
                result.time.as_millis(), 
                result.reading[0],
                result.reading[1],
                result.reading[2],
                result.reading[3])
        } else {
            let _ = yads::RESULT.wait().await;
        }
    };
}


/// ## UI task
/// 
/// The ui task collects events, e.g. button presses, 
/// updates the output (LED, display) and controls the
/// recording task using an atomic lock.
/// 
/// The ui orchestrates the input/output, sensors and storage 
/// using a state machine. 
/// The allowed states are defines by the enum AppState
/// Down the line we have to make copies of the AppState. This is why 
/// a couple of traits need to be derived for AppState.

#[derive(Debug,  // used as fmt
    Clone, Copy, // because next_state 
    PartialEq, Eq, )] // testing equality
enum AppState {New, Ready, Record}

/// In a usual multi-threaded app you would write the interaction model
/// in the main task. However, with dual-core the main task is no longer 
/// async. Since all communication channels are static, this really doesn't matter.
/// 
/// The initial state is set and a signal is send to the LED.
/// The event loop waits for button events (long or short press) 
/// and changes the states, accordingly.
/// If an actual state change has occured, the state is signaled to the UI 
/// and initialized if that is necessary. In this case, entering Record 
/// starts the sensor sampling.
/// 
/// From an architectural point of view, this is a nice setup, too. 
/// Basically, we are separating the very different tasks of 
/// peripherals/spawning and ui handling. It would be easy to just plugin a 
/// different ui, by just reqriting this task. For example, another ui
/// could use the RGB led to signal states, or collect commands from a serial console.
///
/// Conclusion so far: If we take the Embassy promise for granted, that async is zero-cost, 
/// the separation of functionality into different tasks reduces dependencies. It introduces 
/// the complexity of signalling.
 
static SAMPLE: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
async fn ui_task() { 
    let mut state = AppState::New;
    yled::LED.signal(yled::State::Off);
    let disp_text: ydsp::FourLines = [ "YLab".into(), "".into(), "".into(),"".into()];
    ydsp::TEXT.signal(disp_text);
    loop {
        let btn_1 = ybtn::BTN.wait().await;
        let next_state = 
        match (state, btn_1) {
            (AppState::New,     ybtn::Event::Short) => AppState::Ready,
            (AppState::Ready,   ybtn::Event::Short) => AppState::Record, 
            (AppState::Record,  ybtn::Event::Short) => AppState::Ready,
            (_,                 ybtn::Event::Long)  => AppState::New,
            (_, _) => state,};
        

        if next_state != state {
            match next_state {
                AppState::New       => {
                    yled::LED.signal(yled::State::Vibrate);
                    SAMPLE.store(false, Ordering::Relaxed);
                    RECORD.store(false, Ordering::Relaxed);
                    let disp_text: ydsp::FourLines = [ "New".into(), "".into(), "".into(),"".into()];
                    ydsp::TEXT.signal(disp_text);
                    },
                AppState::Ready     => {
                    yled::LED.signal(yled::State::Blink);
                    yadc::CONTROL.signal(yadc::State::Ready);
                    SAMPLE.store(true, Ordering::Relaxed);
                    RECORD.store(false, Ordering::Relaxed);
                    let disp_text: ydsp::FourLines = [ "Ready".into(), "".into(), "".into(),"".into()];
                    ydsp::TEXT.signal(disp_text);
                    },
                AppState::Record    => {
                    yled::LED.signal(yled::State::Steady);
                    yadc::CONTROL.signal(yadc::State::Record);
                    SAMPLE.store(true, Ordering::Relaxed);
                    RECORD.store(true, Ordering::Relaxed);
                    let disp_text: ydsp::FourLines = [ "Record".into(), "".into(), "".into(),"".into()];
                    ydsp::TEXT.signal(disp_text);
                    }
            }
        state = next_state;
        }
    }


}

///# Main Program
/// 
/// The main task starts by collecting the peripherals, 
/// before they are moved to the individual tasks which are spanwed here.


#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());
    spawn_core1(p.CORE1, unsafe { &mut CORE1_STACK }, move || {
        let executor1 = EXECUTOR1.init(Executor::new());
        executor1.run(|spawner|{
            unwrap!(spawner.spawn(yadc::task(
                p.ADC, 
                p.PIN_26, 
                p.PIN_27, 
                p.PIN_28, 
                p.PIN_29, 
                1500)));
           unwrap!(spawner.spawn(yads::task(
                p.I2C1, 
                p.PIN_3, 
                p.PIN_2, 
                2000)));
            }
        )
    });

    let executor0 = EXECUTOR0.init(Executor::new());
    executor0.run(|spawner| {   
        unwrap!(spawner.spawn(yled::task(p.PIN_25.degrade())));
        unwrap!(spawner.spawn(ydsp::task(p.I2C0, p.PIN_4, p.PIN_5)));
        unwrap!(spawner.spawn(ybtn::task(p.PIN_20.degrade())));
        unwrap!(spawner.spawn(ybsu::task(p.USB)));
        unwrap!(spawner.spawn(ui_task()));
        unwrap!(spawner.spawn(storage_task()));
    });
}


