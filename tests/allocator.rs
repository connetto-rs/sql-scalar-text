use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

use sql_values::{
    parse_bool, parse_date, parse_pg_bytea_hex, parse_time, parse_timestamp, parse_timestamp_tz,
};

// This binary runs one test, so the counter needs atomicity without cross-thread ordering.
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarding unchanged layout to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarding unchanged ptr and layout to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn reset() {
    ALLOC_COUNT.store(0, Ordering::Relaxed);
}

fn alloc_count() -> usize {
    ALLOC_COUNT.load(Ordering::Relaxed)
}

#[test]
fn allocation_contract() {
    let timestamps = [
        "2026-01-01 00:00:00",
        "2026-01-01T00:00:00",
        "2026-01-01 00:00:00.5",
        "2026-01-01T00:00:00.5",
    ];
    for &s in &timestamps {
        reset();
        let _ = black_box(parse_timestamp(black_box(s)));
        let n = alloc_count();
        assert_eq!(n, 0, "parse_timestamp({s:?}) allocated {n}");
    }

    let timestamps_tz = [
        "2026-01-01 00:00:00+00",
        "2026-01-01 00:00:00+00:00",
        "2026-01-01 00:00:00+0000",
        "2026-01-01 00:00:00Z",
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00+00:00",
        "2025-12-31 22:00:00-02",
        "2026-01-01 02:30:00+02:30",
        "2026-01-01 00:00:00.5+00",
        "2026-01-01T00:00:00.5Z",
        "1900-01-01 00:19:32+00:19:32",
        "10000-01-01 00:00:00+00",
        "0001-12-31 23:30:00+00 BC",
    ];
    for &s in &timestamps_tz {
        reset();
        let _ = black_box(parse_timestamp_tz(black_box(s)));
        let n = alloc_count();
        assert_eq!(n, 0, "parse_timestamp_tz({s:?}) allocated {n}");
    }

    let dates = ["2026-01-01"];
    for &s in &dates {
        reset();
        let _ = black_box(parse_date(black_box(s)));
        let n = alloc_count();
        assert_eq!(n, 0, "parse_date({s:?}) allocated {n}");
    }

    let times = ["00:00:00", "12:34:56.789"];
    for &s in &times {
        reset();
        let _ = black_box(parse_time(black_box(s)));
        let n = alloc_count();
        assert_eq!(n, 0, "parse_time({s:?}) allocated {n}");
    }

    let bools = ["t", "f", "1", "0"];
    for &s in &bools {
        reset();
        let _ = black_box(parse_bool(black_box(s)));
        let n = alloc_count();
        assert_eq!(n, 0, "parse_bool({s:?}) allocated {n}");
    }

    reset();
    let empty = black_box(parse_pg_bytea_hex(black_box(r"\x")));
    let n = alloc_count();
    assert_eq!(empty, Some(vec![]), "empty bytea wrong value");
    assert_eq!(n, 0, "empty bytea allocated {n}");

    let nonempty: &[&str] = &[r"\x00", r"\x0001ff", r"\xABCDEF"];
    for &s in nonempty {
        reset();
        let result = black_box(parse_pg_bytea_hex(black_box(s)));
        let n = alloc_count();
        assert!(result.is_some(), "parse_pg_bytea_hex({s:?}) returned None");
        assert_eq!(n, 1, "parse_pg_bytea_hex({s:?}) allocated {n}");
    }
}
