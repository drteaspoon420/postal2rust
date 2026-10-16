//! Built-in sampling profiler (Windows, `P2GAME_SAMPLE=out\perf\samples.txt`): a background thread
//! suspends each registered thread ~1000 times a second, unwinds its stack with the x64 unwind
//! tables (RtlVirtualUnwind) and counts the return addresses. At exit the counts are written as
//! `thread<TAB>rva;rva;...<TAB>count` lines (leaf first, RVAs relative to the exe image), to be
//! symbolized with `tools\perf\sample_report.py` (llvm-symbolizer). No admin rights needed, unlike
//! ETW-based profilers. Off unless the env var is set: no thread, no cost.

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    type Handle = isize;
    unsafe extern "system" {
        fn GetCurrentThread() -> Handle;
        fn GetCurrentProcess() -> Handle;
        fn DuplicateHandle(sp: Handle, sh: Handle, tp: Handle, th: *mut Handle, access: u32, inherit: i32, opts: u32) -> i32;
        fn SuspendThread(h: Handle) -> u32;
        fn ResumeThread(h: Handle) -> u32;
        fn GetThreadContext(h: Handle, ctx: *mut u8) -> i32;
        fn GetModuleHandleW(name: *const u16) -> usize;
        fn RtlLookupFunctionEntry(pc: u64, image_base: *mut u64, history: *mut u8) -> *mut u8;
        fn RtlVirtualUnwind(kind: u32, image_base: u64, pc: u64, entry: *mut u8, ctx: *mut u8, handler_data: *mut *mut u8, establisher: *mut u64, nv: *mut u8) -> *mut u8;
        fn timeBeginPeriod(ms: u32) -> u32;
    }

    /// x64 CONTEXT: 1232 bytes, 16-byte aligned.
    #[repr(C, align(16))]
    struct Ctx([u8; 1232]);
    const OFF_FLAGS: usize = 0x30;
    const OFF_RSP: usize = 0x98;
    const OFF_RIP: usize = 0xF8;
    const CONTEXT_FULL: u32 = 0x10000B;

    impl Ctx {
        fn get(&self, off: usize) -> u64 {
            u64::from_le_bytes(self.0[off..off + 8].try_into().unwrap())
        }
        fn set(&mut self, off: usize, v: u64) {
            self.0[off..off + 8].copy_from_slice(&v.to_le_bytes());
        }
    }

    struct Thread {
        name: String,
        handle: Handle,
    }
    // Raw thread handles; only used from the sampler thread.
    unsafe impl Send for Thread {}

    static THREADS: Mutex<Vec<Thread>> = Mutex::new(Vec::new());
    static COUNTS: Mutex<Option<HashMap<(usize, Vec<u32>), u32>>> = Mutex::new(None);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn enabled() -> bool {
        std::env::var_os("P2GAME_SAMPLE").is_some()
    }

    /// Registers the calling thread for sampling (once per thread).
    pub fn register_current(name: &str) {
        if !enabled() {
            return;
        }
        let mut h: Handle = 0;
        // THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION
        let ok = unsafe { DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &mut h, 0x0002 | 0x0008 | 0x0040, 0, 0) };
        if ok != 0 {
            THREADS.lock().unwrap().push(Thread { name: name.to_string(), handle: h });
        }
        start();
    }

    fn start() {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        *COUNTS.lock().unwrap() = Some(HashMap::new());
        unsafe { timeBeginPeriod(1) };
        std::thread::Builder::new()
            .name("p2game sampler".into())
            .spawn(|| {
                let base = unsafe { GetModuleHandleW(std::ptr::null()) } as u64;
                let mut ctx = Box::new(Ctx([0; 1232]));
                let mut stack: Vec<u32> = Vec::with_capacity(64);
                // `P2GAME_SAMPLE_DELAY=secs`: skip the load (default 8 s) so the profile is steady play.
                let delay = std::env::var("P2GAME_SAMPLE_DELAY").ok().and_then(|s| s.trim().parse::<f32>().ok()).unwrap_or(8.0);
                let t0 = std::time::Instant::now();
                while RUNNING.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    if t0.elapsed().as_secs_f32() < delay {
                        continue;
                    }
                    let threads = THREADS.lock().unwrap();
                    for (ti, t) in threads.iter().enumerate() {
                        stack.clear();
                        unsafe {
                            if SuspendThread(t.handle) == u32::MAX {
                                continue;
                            }
                            ctx.0.fill(0);
                            ctx.0[OFF_FLAGS..OFF_FLAGS + 4].copy_from_slice(&CONTEXT_FULL.to_le_bytes());
                            if GetThreadContext(t.handle, ctx.0.as_mut_ptr()) != 0 {
                                for _ in 0..48 {
                                    let pc = ctx.get(OFF_RIP);
                                    if pc == 0 {
                                        break;
                                    }
                                    let rva = pc.wrapping_sub(base);
                                    // Frames outside the exe (system DLLs, driver) count as 0.
                                    stack.push(if rva < 0x1_0000_0000 { rva as u32 } else { 0 });
                                    let mut img: u64 = 0;
                                    let entry = RtlLookupFunctionEntry(pc, &mut img, std::ptr::null_mut());
                                    if entry.is_null() {
                                        // Leaf function: the return address is at [rsp].
                                        let rsp = ctx.get(OFF_RSP);
                                        if rsp == 0 {
                                            break;
                                        }
                                        let ra = *(rsp as *const u64);
                                        ctx.set(OFF_RIP, ra);
                                        ctx.set(OFF_RSP, rsp + 8);
                                    } else {
                                        let mut hd: *mut u8 = std::ptr::null_mut();
                                        let mut est: u64 = 0;
                                        RtlVirtualUnwind(0, img, pc, entry, ctx.0.as_mut_ptr(), &mut hd, &mut est, std::ptr::null_mut());
                                    }
                                }
                            }
                            ResumeThread(t.handle);
                        }
                        if !stack.is_empty() {
                            if let Some(m) = COUNTS.lock().unwrap().as_mut() {
                                *m.entry((ti, stack.clone())).or_insert(0) += 1;
                            }
                        }
                    }
                }
            })
            .ok();
    }

    /// Stops sampling and writes the counts (called at exit).
    pub fn finish() {
        if !RUNNING.swap(false, Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        let Some(path) = std::env::var_os("P2GAME_SAMPLE") else { return };
        let Some(m) = COUNTS.lock().unwrap().take() else { return };
        let threads = THREADS.lock().unwrap();
        let mut out = String::new();
        for ((ti, st), n) in m {
            let name = threads.get(ti).map_or("?", |t| t.name.as_str());
            let s: Vec<String> = st.iter().map(|r| format!("{r:x}")).collect();
            out.push_str(&format!("{name}\t{}\t{n}\n", s.join(";")));
        }
        let _ = std::fs::write(&path, out);
        println!("sampler: wrote {}", std::path::Path::new(&path).display());
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn register_current(_: &str) {}
    pub fn finish() {}
}

pub use imp::{finish, register_current};
