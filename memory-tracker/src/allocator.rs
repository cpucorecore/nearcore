use arr_macro::arr;
use libc;
use log::info;
use rand::Rng;
use std::alloc::{GlobalAlloc, Layout};
use std::cell::RefCell;
use std::cmp::min;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

const ALLOC_LIMIT: usize = 100;

const COUNTERS_SIZE: usize = 16384;
static JEMALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;
static MEM_SIZE: [AtomicUsize; COUNTERS_SIZE as usize] = arr![AtomicUsize::new(0); 16384];
static MEM_CNT: [AtomicUsize; COUNTERS_SIZE as usize] = arr![AtomicUsize::new(0); 16384];
static ENABLED: AtomicUsize = AtomicUsize::new(0);
static SANITY: AtomicUsize = AtomicUsize::new(0);
static mut FILENAME: [u8; 32] = [0; 32];

static mut SKIP_PTR: [u8; 1 << 20] = [0; 1 << 20];
static mut CHECKED_PTR: [u8; 1 << 20] = [0; 1 << 20];

static mut ADDR: Option<*mut c_void> = None;

pub struct MyAllocator;

const EXTRA: usize = 32;
const MAGIC: usize = 0x12345678991124;

thread_local! {
    pub static TID: RefCell<usize> = RefCell::new(usize::max_value());
    pub static TID2: RefCell<usize> = RefCell::new(usize::max_value());
    pub static IN_TRACE: RefCell<usize> = RefCell::new(0);
    pub static LAST_SIZE: RefCell<usize> = RefCell::new(0);
}

extern "C" {
    pub fn gettid() -> u32;
}

pub fn get_tid() -> usize {
    SANITY.fetch_add(1, Ordering::SeqCst);
    let res = TID.with(|t| {
        if *t.borrow() == usize::max_value() {
            *t.borrow_mut() = unsafe{ gettid() as usize };
        }
        *t.borrow()
    });
    SANITY.fetch_sub(1, Ordering::SeqCst);
    res
}


pub fn murmur64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.overflowing_mul(0xff51afd7ed558ccd).0;
    h ^= h >> 33;
    h = h.overflowing_mul(0xc4ceb9fe1a85ec53).0;
    h ^= h >> 33;
    return h;
}

const IGNORE_START: &'static [&'static str] = &[
    "__rg_",
    "_ZN5actix",
    "_ZN5alloc",
    "_ZN6base64",
    "_ZN6cached",
    "_ZN4core",
    "_ZN9hashbrown",
    "_ZN20reed_solomon_erasure",
    "_ZN5tokio",
    "_ZN10tokio_util",
    "_ZN3std",
    "_ZN8smallvec",
];

const IGNORE_INSIDE: &'static [&'static str] = &[
    "$LT$actix..",
    "$LT$alloc..",
    "$LT$base64..",
    "$LT$cached..",
    "$LT$core..",
    "$LT$hashbrown..",
    "$LT$reed_solomon_erasure..",
    "$LT$tokio..",
    "$LT$tokio_util..",
    "$LT$serde_json..",
    "$LT$std..",
    "$LT$tracing_subscriber..",
];

fn skip_ptr(addr: *mut c_void) -> bool {
    if addr as u64 > 0x700000000000 {
        return true;
    }
    let mut found = false;
    backtrace::resolve(addr, |symbol| {
        if let Some(name) = symbol.name() {
            let name = name.as_str().unwrap_or("");
            for &s in IGNORE_START {
                if name.starts_with(s) {
                    found = true;
                    break;
                }
            }
            for &s in IGNORE_INSIDE {
                if name.contains(s) {
                    found = true;
                    break;
                }
            }
        }
    });

    return found;
}

unsafe impl GlobalAlloc for MyAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        SANITY.fetch_add(1, Ordering::SeqCst);

        let new_layout = Layout::from_size_align(layout.size() + EXTRA, layout.align()).unwrap();

        let tid = get_tid();

        let res = JEMALLOC.alloc(new_layout);
        MEM_SIZE[tid % COUNTERS_SIZE].fetch_add(layout.size(), Ordering::SeqCst);
        MEM_CNT[tid % COUNTERS_SIZE].fetch_add(1, Ordering::SeqCst);

        let mut addr: Option<*mut c_void> = Some(2 as *mut c_void);


        let ary: [*mut c_void; 30] = [0 as *mut c_void; 30];

        if IN_TRACE.with(|in_trace| *in_trace.borrow()) == 0 {
            IN_TRACE.with(|in_trace| *in_trace.borrow_mut() = 1);
            if layout.size() >= ALLOC_LIMIT
                || rand::thread_rng().gen_range(0, 100) < 1
//                || LAST_SIZE.with(|ls| *ls.borrow()) != layout.size()
            {
                let size = libc::backtrace(ary.as_ptr() as *mut *mut c_void, 30);
                for i in 1..min(size as usize, 30) {
                    addr = Some(ary[i] as *mut c_void);
                    if ary[i] < 0x700000000000 as *mut c_void {
                        let hash = murmur64(ary[i] as u64) % (1 << 23);
                        if (SKIP_PTR[(hash / 8) as usize] >> hash % 8) & 1 == 1 {
                            continue;
                        }
                        if (CHECKED_PTR[(hash / 8) as usize] >> hash % 8) & 1 == 1 {
                            break;
                        }
                        backtrace::resolve(ary[i], |symbol| {
                            let fname = format!("/tmp/logs/{}", tid);
                            if let Ok(mut f) =
                            OpenOptions::new().create(true).write(true).append(true).open(fname)
                            {
                                if let Some(path) = symbol.filename() {
                                    f.write(format!("PATH {:?} {} {}\n", ary[i], symbol.lineno().unwrap_or(0), path.to_str().unwrap_or("<UNKNOWN>")).as_bytes()).unwrap();
                                }
                                if let Some(name) = symbol.name() {
                                    f.write(format!("SYMBOL {:?} {}\n", ary[i], name.to_string()).as_bytes()).unwrap();
                                }
                            }
                        });


                        let should_skip = skip_ptr(ary[i]);
                        if should_skip {
                            SKIP_PTR[(hash / 8) as usize] |= 1 << hash % 8;
                            continue;
                        }
                        CHECKED_PTR[(hash / 8) as usize] |= 1 << hash % 8;



                        let fname = format!("/tmp/logs/{}", tid);

                        if let Ok(mut f) =
                            OpenOptions::new().create(true).write(true).append(true).open(fname)
                        {
                            f.write(format!("STACK_FOR {:?}\n", addr).as_bytes()).unwrap();
                            let ary2: [*mut c_void; 256] = [0 as *mut c_void; 256];
                            let size2 =
                                libc::backtrace(ary2.as_ptr() as *mut *mut c_void, 256) as usize;
                            for i in 0..size2 {
                                let addr2 = ary2[i];

                                backtrace::resolve(addr2, |symbol| {
                                    if let Some(name) = symbol.name() {
                                        let name = name.as_str().unwrap_or("");

                                        f.write(
                                            format!("STACK {:?} {:?} {:?}\n", i, addr2, name)
                                                .as_bytes(),
                                        )
                                        .unwrap();
                                    }
                                });
                            }
                        }
                        break;
                    }
                }
            } else {
                addr = Some(1 as *mut c_void);
            }
            IN_TRACE.with(|in_trace| *in_trace.borrow_mut() = 0);
            LAST_SIZE.with(|ls| *ls.borrow_mut() = layout.size());
        }

        ADDR = addr;

        *(res as *mut u64) = MAGIC as u64;
        *(res as *mut u64).offset(1) = layout.size() as u64;
        *(res as *mut u64).offset(2) = tid as u64;
        *(res as *mut *mut c_void).offset(3) = addr.unwrap_or(0 as *mut c_void);
        SANITY.fetch_sub(1, Ordering::SeqCst);
        res.offset(32)
    }

    unsafe fn dealloc(&self, mut ptr: *mut u8, layout: Layout) {
        SANITY.fetch_add(1, Ordering::SeqCst);
        let new_layout = Layout::from_size_align(layout.size() + EXTRA, layout.align()).unwrap();

        ptr = ptr.offset(-32);

        *(ptr as *mut u64) = (MAGIC + 0x10) as u64;
        let tid: usize = *(ptr as *mut u64).offset(2) as usize;

        MEM_SIZE[tid % COUNTERS_SIZE].fetch_sub(layout.size(), Ordering::SeqCst);
        MEM_CNT[tid % COUNTERS_SIZE].fetch_sub(1, Ordering::SeqCst);

        JEMALLOC.dealloc(ptr, new_layout);
        SANITY.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn enable_tracking(name: &str) {
    ENABLED.store(1, Ordering::SeqCst);

    TID2.with(|t| {
        if *t.borrow() == usize::max_value() {
            let tid = get_tid();
            info!("enabling tracking for {}: {}", name, tid);
            *t.borrow_mut() = tid;
        }
    });
}

pub fn print_counters_ary() {
    info!(
        "HMM {} {:?}",
        unsafe { ADDR.unwrap_or(0 as *mut c_void) as u64 },
        std::str::from_utf8(unsafe { FILENAME.as_ref() })
    );
    info!("tid {}", get_tid());
    let mut total_cnt: usize = 0;
    let mut total_size: usize = 0;
    for idx in 0..COUNTERS_SIZE {
        let val: usize = MEM_SIZE.get(idx).unwrap().load(Ordering::SeqCst);
        if val != 0 {
            let cnt = MEM_CNT.get(idx).unwrap().load(Ordering::SeqCst);
            total_cnt += cnt;
            info!("COUNTERS {}: {} {}", idx, cnt, val);
            total_size += val;
        }
    }
    info!("COUNTERS TOTAL {} {}", total_cnt, total_size);
}

pub fn get_sanity_val() -> usize {
    SANITY.load(Ordering::SeqCst)
}
