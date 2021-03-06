use std::cell::RefCell;
use std::str::{self, FromStr};
use std::rc::Rc;
use std::io;
use std::fs::File;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::env;
use std::process;
use log::{LogLevel, LogLevelFilter, LogRecord};
use env_logger::LogBuilder;
use getopts::{Options, Matches};

use smoltcp::phy::{Device, EthernetTracer, FaultInjector, TapInterface};
use smoltcp::phy::{PcapWriter, PcapSink, PcapMode, PcapLinkType};

pub fn setup_logging_with_clock<F>(filter: &str, since_startup: F)
        where F: Fn() -> u64 + Send + Sync + 'static {
    LogBuilder::new()
        .format(move |record: &LogRecord| {
            let elapsed = since_startup();
            let timestamp = format!("[{:6}.{:03}s]", elapsed / 1000, elapsed % 1000);
            if record.target().starts_with("smoltcp::") {
                format!("\x1b[0m{} ({}): {}\x1b[0m", timestamp,
                        record.target().replace("smoltcp::", ""), record.args())
            } else if record.level() == LogLevel::Trace {
                let mut message = format!("{}", record.args());
                message.pop();
                format!("\x1b[37m{} {}\x1b[0m", timestamp,
                        message.replace("\n", "\n             "))
            } else {
                format!("\x1b[32m{} ({}): {}\x1b[0m", timestamp,
                        record.target(), record.args())
            }
        })
        .filter(None, LogLevelFilter::Trace)
        .parse(filter)
        .parse(&env::var("RUST_LOG").unwrap_or("".to_owned()))
        .init()
        .unwrap();
}

pub fn setup_logging(filter: &str) {
    let startup_at = Instant::now();
    setup_logging_with_clock(filter, move  || {
        let elapsed = Instant::now().duration_since(startup_at);
        elapsed.as_secs() * 1000 + (elapsed.subsec_nanos() / 1000000) as u64
    })
}

struct Dispose;

impl io::Write for Dispose {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn create_options() -> (Options, Vec<&'static str>) {
    let mut opts = Options::new();
    opts.optflag("h", "help", "print this help menu");
    (opts, Vec::new())
}

pub fn parse_options(options: &Options, free: Vec<&str>) -> Matches {
    match options.parse(env::args().skip(1)) {
        Err(err) => {
            println!("{}", err);
            process::exit(1)
        }
        Ok(matches) => {
            if matches.opt_present("h") || matches.free.len() != free.len() {
                let brief = format!("Usage: {} [OPTION]... {}",
                                    env::args().nth(0).unwrap(), free.join(" "));
                print!("{}", options.usage(&brief));
                process::exit(if matches.free.len() != free.len() { 1 } else { 0 })
            }
            matches
        }
    }
}

pub fn add_tap_options(_opts: &mut Options, free: &mut Vec<&str>) {
    free.push("INTERFACE");
}

pub fn parse_tap_options(matches: &mut Matches) -> TapInterface {
    let interface = matches.free.remove(0);
    TapInterface::new(&interface).unwrap()
}

pub fn add_middleware_options(opts: &mut Options, _free: &mut Vec<&str>) {
    opts.optopt("", "pcap", "Write a packet capture file", "FILE");
    opts.optopt("", "drop-chance", "Chance of dropping a packet (%)", "CHANCE");
    opts.optopt("", "corrupt-chance", "Chance of corrupting a packet (%)", "CHANCE");
    opts.optopt("", "size-limit", "Drop packets larger than given size (octets)", "SIZE");
    opts.optopt("", "tx-rate-limit", "Drop packets after transmit rate exceeds given limit \
                                      (packets per interval)", "RATE");
    opts.optopt("", "rx-rate-limit", "Drop packets after transmit rate exceeds given limit \
                                      (packets per interval)", "RATE");
    opts.optopt("", "shaping-interval", "Sets the interval for rate limiting (ms)", "RATE");
}

pub fn parse_middleware_options<D: Device>(matches: &mut Matches, device: D, loopback: bool)
        -> FaultInjector<EthernetTracer<PcapWriter<D, Rc<PcapSink>>>> {
    let drop_chance      = matches.opt_str("drop-chance").map(|s| u8::from_str(&s).unwrap())
                                  .unwrap_or(0);
    let corrupt_chance   = matches.opt_str("corrupt-chance").map(|s| u8::from_str(&s).unwrap())
                                  .unwrap_or(0);
    let size_limit       = matches.opt_str("size-limit").map(|s| usize::from_str(&s).unwrap())
                                  .unwrap_or(0);
    let tx_rate_limit    = matches.opt_str("tx-rate-limit").map(|s| u64::from_str(&s).unwrap())
                                  .unwrap_or(0);
    let rx_rate_limit    = matches.opt_str("rx-rate-limit").map(|s| u64::from_str(&s).unwrap())
                                  .unwrap_or(0);
    let shaping_interval = matches.opt_str("shaping-interval").map(|s| u64::from_str(&s).unwrap())
                                  .unwrap_or(0);

    let pcap_writer: Box<io::Write>;
    if let Some(pcap_filename) = matches.opt_str("pcap") {
        pcap_writer = Box::new(File::create(pcap_filename).expect("cannot open file"))
    } else {
        pcap_writer = Box::new(Dispose)
    }

    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos();

    let device = PcapWriter::new(device, Rc::new(RefCell::new(pcap_writer)) as Rc<PcapSink>,
                                 if loopback { PcapMode::TxOnly } else { PcapMode::Both },
                                 PcapLinkType::Ethernet);
    let device = EthernetTracer::new(device, |_timestamp, printer| trace!("{}", printer));
    let mut device = FaultInjector::new(device, seed);
    device.set_drop_chance(drop_chance);
    device.set_corrupt_chance(corrupt_chance);
    device.set_max_packet_size(size_limit);
    device.set_max_tx_rate(tx_rate_limit);
    device.set_max_rx_rate(rx_rate_limit);
    device.set_bucket_interval(shaping_interval);
    device
}
